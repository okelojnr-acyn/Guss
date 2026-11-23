use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use guss_core::store::TimeWindow;
use thiserror::Error;
use tokio_cron_scheduler::{Job, JobScheduler};

#[derive(Debug, Error)]
pub enum SchedulerError {
    #[error("scheduler error: {0}")]
    Scheduler(String),
}

/// Where a scheduled report gets delivered - one implementation per
/// channel that supports proactive pushes (Telegram, WhatsApp both have
/// a persistent addressable chat; CLI doesn't, so it has no sink).
#[async_trait]
pub trait ReportSink: Send + Sync {
    async fn send_report(&self, text: String) -> Result<(), String>;
}

/// Cron expressions for each report cadence, in the 6-field
/// sec-min-hour-day-month-weekday syntax `tokio-cron-scheduler` expects.
pub struct ScheduleConfig {
    pub daily_cron: String,
    pub weekly_cron: String,
    pub monthly_cron: String,
}

impl Default for ScheduleConfig {
    fn default() -> Self {
        Self {
            daily_cron: "0 0 20 * * *".to_string(),   // every day at 20:00
            weekly_cron: "0 0 9 * * Mon".to_string(), // Monday 09:00
            monthly_cron: "0 0 9 1 * *".to_string(),  // 1st of the month, 09:00
        }
    }
}

/// Builds the report text for a window. The app hands in Guss's
/// orchestrator, acting as the owner, so scheduled reports match what the
/// owner gets on asking (including Friday's tills when linked).
#[async_trait]
pub trait ReportSource: Send + Sync {
    async fn report(&self, window: TimeWindow) -> Result<String, String>;
}

/// Starts the periodic report scheduler. Returns the running
/// `JobScheduler` - keep it alive (e.g. hold it in a variable for the
/// life of the process) or the jobs stop firing when it's dropped.
pub async fn start(
    source: Arc<dyn ReportSource>,
    sink: Arc<dyn ReportSink>,
    schedule: ScheduleConfig,
) -> Result<JobScheduler, SchedulerError> {
    let scheduler = JobScheduler::new()
        .await
        .map_err(|e| SchedulerError::Scheduler(e.to_string()))?;
    for (cron, window) in [
        (&schedule.daily_cron, TimeWindow::Day),
        (&schedule.weekly_cron, TimeWindow::Week),
        (&schedule.monthly_cron, TimeWindow::Month),
    ] {
        add_job(&scheduler, cron, window, source.clone(), sink.clone()).await?;
    }
    scheduler
        .start()
        .await
        .map_err(|e| SchedulerError::Scheduler(e.to_string()))?;
    Ok(scheduler)
}

async fn add_job(
    scheduler: &JobScheduler,
    cron_expr: &str,
    window: TimeWindow,
    source: Arc<dyn ReportSource>,
    sink: Arc<dyn ReportSink>,
) -> Result<(), SchedulerError> {
    // In-flight guard: if a previous tick for this job is still running
    // (e.g. a slow LLM/report render, or the sink's HTTP call stalling),
    // skip-and-log this tick rather than letting runs pile up.
    let in_flight = Arc::new(AtomicBool::new(false));

    let job = Job::new_async(cron_expr, move |_uuid, _lock| {
        let source = source.clone();
        let sink = sink.clone();
        let in_flight = in_flight.clone();
        Box::pin(async move {
            if in_flight.swap(true, Ordering::SeqCst) {
                tracing::warn!(
                    ?window,
                    "previous scheduled report run hasn't finished yet - skipping this tick"
                );
                return;
            }
            let sent = match source.report(window).await {
                Ok(text) => sink.send_report(text).await,
                Err(e) => Err(e),
            };
            if let Err(e) = sent {
                tracing::warn!(error = %e, ?window, "scheduled report run failed");
            }
            in_flight.store(false, Ordering::SeqCst);
        })
    })
    .map_err(|e| SchedulerError::Scheduler(e.to_string()))?;

    scheduler
        .add(job)
        .await
        .map_err(|e| SchedulerError::Scheduler(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Shop;

    #[async_trait]
    impl ReportSource for Shop {
        async fn report(&self, _window: TimeWindow) -> Result<String, String> {
            Ok("Report for Acme Shop".into())
        }
    }

    struct RecordingSink {
        received: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl ReportSink for RecordingSink {
        async fn send_report(&self, text: String) -> Result<(), String> {
            self.received.lock().unwrap().push(text);
            Ok(())
        }
    }

    #[tokio::test]
    async fn job_fires_and_delivers_a_report_on_a_tight_schedule() {
        let sink = Arc::new(RecordingSink {
            received: Mutex::new(Vec::new()),
        });

        let mut scheduler = start(
            Arc::new(Shop),
            sink.clone(),
            ScheduleConfig {
                daily_cron: "* * * * * *".to_string(), // every second, test-only
                weekly_cron: "0 0 9 * * Mon".to_string(),
                monthly_cron: "0 0 9 1 * *".to_string(),
            },
        )
        .await
        .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
        scheduler.shutdown().await.ok();

        let received = sink.received.lock().unwrap();
        assert!(
            !received.is_empty(),
            "expected at least one scheduled report to fire within 2.5s"
        );
        assert!(received[0].contains("Acme Shop"));
    }
}
