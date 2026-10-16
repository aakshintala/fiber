use super::*;

struct StubJobs;

impl Jobs for StubJobs {
    fn open(&self, _opening: Opening) -> Result<Opened, OpenError> {
        Err(OpenError::Io {
            path: std::path::PathBuf::from("/none"),
            source: std::io::Error::other("refused"),
        })
    }

    fn stop(&self, _job_id: &crate::JobId) -> bool {
        false
    }

    fn background(&self) -> usize {
        0
    }

    fn foreground(&self, _call: Foreground) {}

    fn running(&self) -> Vec<crate::JobId> {
        Vec::new()
    }

    fn stop_delegates(&self) -> usize {
        0
    }

    fn deliver_to(&self, _inbox: std::sync::mpsc::Sender<crate::inbox::Delivery>) {}
}

#[test]
fn default_write_reports_not_running() {
    let jobs = StubJobs;
    assert!(
        matches!(
            jobs.write(&crate::JobId("j_1".into()), "x"),
            Err(WriteError::NotRunning)
        ),
        "default write did not report the job as not running"
    );
}
