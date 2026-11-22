use std::time::Duration;

use fpgaflow_core::job::{self, JobEvent, JobSpec, Outcome, Stream};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn sh(script: &str) -> JobSpec {
    JobSpec::new("test", "/bin/sh", std::env::temp_dir()).args(["-c", script])
}

fn lines(rx: &mut mpsc::UnboundedReceiver<JobEvent>) -> Vec<(Stream, String)> {
    let mut v = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        if let JobEvent::Line { stream, text, .. } = ev {
            v.push((stream, text));
        }
    }
    v
}

#[tokio::test]
async fn streams_both_pipes_and_reports_exit_code() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let rec = job::run(
        &sh("echo out1; echo err1 >&2; printf 'a\\rb\\n'; exit 3"),
        &tx,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(rec.outcome, Outcome::Exited { code: 3 });
    assert!(!rec.outcome.success());
    let got = lines(&mut rx);
    assert!(got.contains(&(Stream::Stdout, "out1".into())));
    assert!(got.contains(&(Stream::Stderr, "err1".into())));
    assert!(
        got.contains(&(Stream::Stdout, "b".into())),
        "carriage-return segment: {got:?}"
    );
}

#[tokio::test]
async fn cancel_kills_the_whole_process_group() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    // A grandchild `sleep` would keep stdout open forever if only the shell were killed.
    let spec = sh("echo started; sleep 60 & wait");
    let handle = tokio::spawn(async move { job::run(&spec, &tx, &c2).await });
    // Wait for the job to report it has started, then cancel.
    loop {
        match rx.recv().await.expect("job ended before starting") {
            JobEvent::Line { text, .. } if text == "started" => break,
            _ => {}
        }
    }
    cancel.cancel();
    let rec = tokio::time::timeout(Duration::from_secs(10), handle)
        .await
        .expect("cancel hung")
        .unwrap()
        .unwrap();
    assert_eq!(rec.outcome, Outcome::Cancelled);
}

#[tokio::test]
async fn timeout_is_enforced() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let spec = sh("sleep 60").timeout(Duration::from_millis(200));
    let rec = tokio::time::timeout(
        Duration::from_secs(10),
        job::run(&spec, &tx, &CancellationToken::new()),
    )
    .await
    .expect("timeout hung")
    .unwrap();
    assert_eq!(rec.outcome, Outcome::TimedOut);
}

#[tokio::test]
async fn missing_program_is_an_error() {
    let (tx, _rx) = mpsc::unbounded_channel();
    let spec = JobSpec::new("x", "/nonexistent/tool", std::env::temp_dir());
    assert!(job::run(&spec, &tx, &CancellationToken::new())
        .await
        .is_err());
}

#[test]
fn run_record_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let rec = job::RunRecord {
        job: "j".into(),
        command: "true".into(),
        cwd: dir.path().into(),
        outcome: Outcome::Exited { code: 0 },
        duration_ms: 1,
        finished_unix: 0,
    };
    rec.write(&dir.path().join("stage")).unwrap();
    let text = std::fs::read_to_string(dir.path().join("stage/run.json")).unwrap();
    assert!(text.contains("\"exited\""));
}
