//! Items and units of measure: one name per item whatever the spelling
//! ("T-Shirts", "tshirt"), and quantities converted to the item's base unit
//! ("2 crates" of soda counted in bottles). Pure logic; the orchestrator
//! loads and saves the catalog.

use guss_core::domain::{BusinessEvent, Item};

/// Lower-case, no punctuation, last word made singular:
/// "T-Shirts" -> "tshirt", "Bags of Rice" -> "bags of rice".
pub fn normalize_name(raw: &str) -> String {
    let cleaned: String = raw
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect();
    let words: Vec<&str> = cleaned.split_whitespace().collect();
    match words.split_last() {
        Some((last, rest)) => {
            let mut out: Vec<String> = rest.iter().map(|w| w.to_string()).collect();
            out.push(singular(last));
            out.join(" ")
        }
        None => String::new(),
    }
}

fn singular(word: &str) -> String {
    if word.len() > 4 && word.ends_with("ies") {
        format!("{}y", &word[..word.len() - 3])
    } else if word.len() > 4
        && ["ses", "xes", "ches", "shes"]
            .iter()
            .any(|e| word.ends_with(e))
    {
        word[..word.len() - 2].to_string()
    } else if word.len() > 3 && word.ends_with('s') && !word.ends_with("ss") {
        word[..word.len() - 1].to_string()
    } else {
        word.to_string()
    }
}

/// One spelling per unit: "Kgs", "kilo" -> "kg"; "ltrs" -> "litre";
/// "pcs" -> "piece"; others lower-case and singular ("crates" -> "crate").
pub fn normalize_unit(raw: &str) -> String {
    let u = raw.trim().to_lowercase();
    let u = u.trim_end_matches('.');
    match u {
        "kg" | "kgs" | "kilo" | "kilos" | "kilogram" | "kilograms" => "kg".into(),
        "g" | "gm" | "gms" | "gram" | "grams" => "g".into(),
        "l" | "ltr" | "ltrs" | "litre" | "litres" | "liter" | "liters" => "litre".into(),
        "ml" | "mls" => "ml".into(),
        "pc" | "pcs" | "piece" | "pieces" => "piece".into(),
        other => normalize_name(other),
    }
}

/// Conversions that are true for every item, so they are never asked.
fn builtin_factor(unit: &str, base: &str) -> Option<f64> {
    const SCALES: &[(&str, &str, f64)] = &[("kg", "g", 1000.0), ("litre", "ml", 1000.0)];
    if unit == base {
        return Some(1.0);
    }
    for (big, small, f) in SCALES {
        if unit == *big && base == *small {
            return Some(*f);
        }
        if unit == *small && base == *big {
            return Some(1.0 / f);
        }
    }
    match unit {
        "dozen" => Some(12.0),
        "pair" => Some(2.0),
        _ => None,
    }
}

/// The unit Guss doesn't know yet for an item: "how many bottles in a crate?"
#[derive(Debug, Clone, PartialEq)]
pub struct UnitQuestion {
    pub item: String,
    pub unit: String,
    pub base: String,
}

pub struct Catalog {
    items: Vec<Item>,
}

impl Catalog {
    pub fn new(items: Vec<Item>) -> Self {
        Self { items }
    }

    /// The item a name refers to, by its name or one of its aliases.
    pub fn find(&self, raw: &str) -> Option<&Item> {
        let n = normalize_name(raw);
        if n.is_empty() {
            return None;
        }
        self.items.iter().find(|i| {
            normalize_name(&i.name) == n || i.aliases.iter().any(|a| normalize_name(a) == n)
        })
    }

    /// Known items for the model's context, so it reuses the shop's names:
    /// "soda (bottle; crate = 24)".
    pub fn context_line(&self, language: &str) -> Option<String> {
        if self.items.is_empty() {
            return None;
        }
        let list: Vec<String> = self
            .items
            .iter()
            .map(|i| {
                let mut parts = Vec::new();
                if let Some(b) = &i.base_unit {
                    parts.push(b.clone());
                }
                for (u, f) in &i.units {
                    parts.push(format!("{u} = {f}"));
                }
                if !i.aliases.is_empty() {
                    parts.push(format!("also: {}", i.aliases.join(", ")));
                }
                if parts.is_empty() {
                    i.name.clone()
                } else {
                    format!("{} ({})", i.name, parts.join("; "))
                }
            })
            .collect();
        Some(match language {
            "id" => format!("Barang toko ini (pakai nama ini): {}", list.join(", ")),
            _ => format!("This shop's items (use these names): {}", list.join(", ")),
        })
    }
}

/// Gives every sale and stock update the shop's item name, a normalised
/// unit and its factor to the base unit. Stops at the first unit Guss
/// can't convert and returns the question to ask; call again after the
/// answer is in the catalog.
pub fn resolve(events: &mut [BusinessEvent], catalog: &Catalog) -> Option<UnitQuestion> {
    for event in events.iter_mut() {
        let (name, unit, factor) = match event {
            BusinessEvent::Sale(s) => (&mut s.item_name, &mut s.unit, &mut s.unit_factor),
            BusinessEvent::StockUpdate(su) => {
                (&mut su.item_name, &mut su.unit, &mut su.unit_factor)
            }
            _ => continue,
        };
        *unit = unit
            .as_deref()
            .map(normalize_unit)
            .filter(|u| !u.is_empty());
        let Some(item) = catalog.find(name) else {
            // A new item: counted in whatever unit it was first given in.
            *name = normalize_name(name);
            *factor = 1.0;
            continue;
        };
        *name = item.name.clone();
        *factor = match (unit.as_deref(), item.base_unit.as_deref()) {
            (None, _) | (Some(_), None) => 1.0,
            (Some(u), Some(base)) => {
                let learned = item.units.iter().find(|(n, _)| n == u).map(|(_, f)| *f);
                match builtin_factor(u, base).or(learned) {
                    Some(f) => f,
                    None => {
                        return Some(UnitQuestion {
                            item: item.name.clone(),
                            unit: u.to_string(),
                            base: base.to_string(),
                        });
                    }
                }
            }
        };
    }
    None
}

/// New items, and base units learned for items that had none, from
/// entries about to be saved.
pub fn learn(events: &[BusinessEvent], catalog: &Catalog) -> Vec<Item> {
    let mut out: Vec<Item> = Vec::new();
    for event in events {
        let (name, unit) = match event {
            BusinessEvent::Sale(s) => (&s.item_name, &s.unit),
            BusinessEvent::StockUpdate(su) => (&su.item_name, &su.unit),
            _ => continue,
        };
        if out.iter().any(|i| &i.name == name) {
            continue;
        }
        match catalog.find(name) {
            None => out.push(Item {
                name: name.clone(),
                base_unit: unit.clone(),
                units: vec![],
                aliases: vec![],
            }),
            Some(item) if item.base_unit.is_none() && unit.is_some() => {
                let mut item = item.clone();
                item.base_unit = unit.clone();
                out.push(item);
            }
            Some(_) => {}
        }
    }
    out
}

/// "24", "24 bottles", "about 24" -> 24.0. Must be more than zero.
pub fn parse_count(reply: &str) -> Option<f64> {
    let start = reply.find(|c: char| c.is_ascii_digit())?;
    let num: String = reply[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    num.parse::<f64>().ok().filter(|n| *n > 0.0)
}

pub fn unit_question_text(q: &UnitQuestion, language: &str) -> String {
    match language {
        "id" => format!(
            "Berapa {base} dalam satu {unit} {item}? Balas dengan angka, misalnya 24. \
             Belum ada yang disimpan.",
            base = q.base,
            unit = q.unit,
            item = q.item
        ),
        _ => format!(
            "How many {base} in one {unit} of {item}? Reply with a number, e.g. 24. \
             Nothing is saved yet.",
            base = q.base,
            unit = q.unit,
            item = q.item
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use guss_core::domain::{Sale, SourceChannel, StockUpdate};
    use guss_core::money::CurrencyAmount;
    use uuid::Uuid;

    fn soda() -> Item {
        Item {
            name: "soda".into(),
            base_unit: Some("bottle".into()),
            units: vec![("crate".into(), 24.0)],
            aliases: vec!["coke".into()],
        }
    }

    fn sale(item: &str, qty: f64, unit: Option<&str>) -> BusinessEvent {
        BusinessEvent::Sale(Sale {
            id: Uuid::new_v4(),
            business_id: Uuid::nil(),
            item_name: item.into(),
            quantity: qty,
            unit: unit.map(str::to_string),
            unit_factor: 1.0,
            unit_price: CurrencyAmount::new(80.0, "KES"),
            category: None,
            occurred_at: Utc::now(),
            recurring: None,
            source_channel: SourceChannel::Cli,
            paid_by: None,
            raw_text: None,
        })
    }

    fn restock(item: &str, delta: f64, unit: Option<&str>) -> BusinessEvent {
        BusinessEvent::StockUpdate(StockUpdate {
            id: Uuid::new_v4(),
            business_id: Uuid::nil(),
            item_name: item.into(),
            delta,
            unit: unit.map(str::to_string),
            unit_factor: 1.0,
            unit_cost: None,
            reorder_threshold: None,
            occurred_at: Utc::now(),
            source_channel: SourceChannel::Cli,
            raw_text: None,
        })
    }

    fn name_unit_factor(e: &BusinessEvent) -> (String, Option<String>, f64) {
        match e {
            BusinessEvent::Sale(s) => (s.item_name.clone(), s.unit.clone(), s.unit_factor),
            BusinessEvent::StockUpdate(su) => {
                (su.item_name.clone(), su.unit.clone(), su.unit_factor)
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn names_and_units_normalise() {
        assert_eq!(normalize_name("T-Shirts"), "tshirt");
        assert_eq!(
            normalize_name("organization Tshirts"),
            "organization tshirt"
        );
        assert_eq!(normalize_name("Batteries"), "battery");
        assert_eq!(normalize_name("boxes"), "box");
        assert_eq!(normalize_name("glass"), "glass");
        assert_eq!(normalize_name("eggs"), "egg");
        assert_eq!(normalize_unit("Kgs"), "kg");
        assert_eq!(normalize_unit("ltrs"), "litre");
        assert_eq!(normalize_unit("crates"), "crate");
        assert_eq!(normalize_unit("pcs"), "piece");
    }

    #[test]
    fn known_unit_converts_and_alias_maps_to_the_item() {
        let catalog = Catalog::new(vec![soda()]);
        let mut events = vec![
            sale("Coke", 2.0, Some("crates")),
            restock("sodas", 5.0, None),
        ];
        assert_eq!(resolve(&mut events, &catalog), None);
        assert_eq!(
            name_unit_factor(&events[0]),
            ("soda".into(), Some("crate".into()), 24.0)
        );
        assert_eq!(name_unit_factor(&events[1]), ("soda".into(), None, 1.0));
    }

    #[test]
    fn builtin_conversions_are_never_asked() {
        let rice = Item {
            name: "rice".into(),
            base_unit: Some("kg".into()),
            units: vec![],
            aliases: vec![],
        };
        let eggs = Item {
            name: "egg".into(),
            base_unit: Some("piece".into()),
            units: vec![],
            aliases: vec![],
        };
        let catalog = Catalog::new(vec![rice, eggs]);
        let mut events = vec![
            sale("rice", 500.0, Some("grams")),
            sale("eggs", 2.0, Some("dozen")),
        ];
        assert_eq!(resolve(&mut events, &catalog), None);
        assert_eq!(name_unit_factor(&events[0]).2, 0.001);
        assert_eq!(name_unit_factor(&events[1]).2, 12.0);
    }

    #[test]
    fn unknown_unit_asks() {
        let catalog = Catalog::new(vec![soda()]);
        let mut events = vec![sale("soda", 1.0, Some("six-pack"))];
        assert_eq!(
            resolve(&mut events, &catalog),
            Some(UnitQuestion {
                item: "soda".into(),
                unit: "sixpack".into(),
                base: "bottle".into()
            })
        );
    }

    #[test]
    fn new_items_are_learned_with_their_first_unit() {
        let catalog = Catalog::new(vec![]);
        let mut events = vec![
            restock("Tshirts", 30.0, None),
            sale("Sugar", 2.0, Some("kgs")),
        ];
        assert_eq!(resolve(&mut events, &catalog), None);
        let learned = learn(&events, &catalog);
        assert_eq!(learned.len(), 2);
        assert_eq!(learned[0].name, "tshirt");
        assert_eq!(learned[0].base_unit, None);
        assert_eq!(learned[1].name, "sugar");
        assert_eq!(learned[1].base_unit.as_deref(), Some("kg"));
    }

    #[test]
    fn counts_parse_from_replies() {
        assert_eq!(parse_count("24"), Some(24.0));
        assert_eq!(parse_count("24 bottles"), Some(24.0));
        assert_eq!(parse_count("about 12.5"), Some(12.5));
        assert_eq!(parse_count("0"), None);
        assert_eq!(parse_count("no idea"), None);
    }
}
