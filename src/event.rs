use std::sync::Arc;
use std::time::Duration;

use bstr::BString;

use crate::app::FileSummary;

#[derive(Debug)]
pub enum WorkerEvent {
    Started {
        path: BString,
        worker_id: usize,
    },
    Finished {
        path: BString,
        summary: FileSummary,
        elapsed: Duration,
    },
    Failed {
        path: BString,
        error: Arc<str>,
        elapsed: Duration,
    },
}
