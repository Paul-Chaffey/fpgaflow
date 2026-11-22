//! Async runner for external tools.
//!
//! Each job runs in its own process group so that cancelling it also stops
//! any children it started (Verilator's make, ISE's helpers, ...). Output is
//! streamed line by line as [`JobEvent`]s so the GUI and CLI can show it live.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

/// How long a cancelled job gets to exit after SIGTERM before SIGKILL.
const TERM_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct JobSpec {
    /// Short name shown in the log, e.g. `sim:build`.
    pub name: String,
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub env: Vec<(OsString, OsString)>,
    /// Hard upper bound on run time; `None` for no limit.
    pub timeout: Option<Duration>,
}

impl JobSpec {
    pub fn new(
        name: impl Into<String>,
        program: impl Into<PathBuf>,
        cwd: impl Into<PathBuf>,
    ) -> Self {
        Self {
            name: name.into(),
            program: program.into(),
            args: Vec::new(),
            cwd: cwd.into(),
            env: Vec::new(),
            timeout: None,
        }
    }

    pub fn arg(mut self, a: impl Into<OsString>) -> Self {
        self.args.push(a.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, k: impl Into<OsString>, v: impl Into<OsString>) -> Self {
        self.env.push((k.into(), v.into()));
        self
    }

    pub fn timeout(mut self, d: Duration) -> Self {
        self.timeout = Some(d);
        self
    }

    /// Shell-quoted command line, for logs and `run.json`.
    pub fn command_line(&self) -> String {
        let mut words = vec![self.program.to_string_lossy().into_owned()];
        words.extend(self.args.iter().map(|a| a.to_string_lossy().into_owned()));
        shell_words::join(words)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Stream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    Exited { code: i32 },
    Signalled,
    Cancelled,
    TimedOut,
}

impl Outcome {
    pub fn success(self) -> bool {
        self == Outcome::Exited { code: 0 }
    }
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Outcome::Exited { code } => write!(f, "exit code {code}"),
            Outcome::Signalled => f.write_str("killed by signal"),
            Outcome::Cancelled => f.write_str("cancelled"),
            Outcome::TimedOut => f.write_str("timed out"),
        }
    }
}

#[derive(Debug, Clone)]
pub enum JobEvent {
    /// A progress message from the flow itself (not from a tool).
    Note {
        text: String,
    },
    Started {
        job: String,
        command: String,
    },
    Line {
        job: String,
        stream: Stream,
        text: String,
    },
    Finished {
        job: String,
        outcome: Outcome,
        duration: Duration,
    },
}

/// What a finished job leaves in `build/<stage>/run.json`.
#[derive(Debug, Clone, Serialize)]
pub struct RunRecord {
    pub job: String,
    pub command: String,
    pub cwd: PathBuf,
    pub outcome: Outcome,
    pub duration_ms: u128,
    pub finished_unix: u64,
}

impl RunRecord {
    pub fn write(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(dir.join("run.json"), text)
    }
}

/// Run one job to completion, cancellation or timeout, streaming its output
/// to `events`. Returns the outcome plus a [`RunRecord`].
///
/// An error is returned only if the process could not be started.
pub async fn run(
    spec: &JobSpec,
    events: &UnboundedSender<JobEvent>,
    cancel: &CancellationToken,
) -> std::io::Result<RunRecord> {
    let command = spec.command_line();
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args)
        .current_dir(&spec.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }

    let start = Instant::now();
    let mut child = cmd.spawn()?;
    let _ = events.send(JobEvent::Started {
        job: spec.name.clone(),
        command: command.clone(),
    });

    let pgid = child.id().map(|p| Pid::from_raw(p as i32));
    let out = child.stdout.take().expect("stdout is piped");
    let err = child.stderr.take().expect("stderr is piped");
    let pump_out = tokio::spawn(pump(spec.name.clone(), Stream::Stdout, out, events.clone()));
    let pump_err = tokio::spawn(pump(spec.name.clone(), Stream::Stderr, err, events.clone()));

    enum Ended {
        Exit(std::io::Result<std::process::ExitStatus>),
        Cancel,
        Timeout,
    }
    let deadline = async {
        match spec.timeout {
            Some(d) => tokio::time::sleep(d).await,
            None => std::future::pending().await,
        }
    };
    let ended = tokio::select! {
        status = child.wait() => Ended::Exit(status),
        _ = cancel.cancelled() => Ended::Cancel,
        _ = deadline => Ended::Timeout,
    };
    let outcome = match ended {
        Ended::Exit(status) => match status?.code() {
            Some(code) => Outcome::Exited { code },
            None => Outcome::Signalled,
        },
        Ended::Cancel => {
            terminate(&mut child, pgid).await;
            Outcome::Cancelled
        }
        Ended::Timeout => {
            terminate(&mut child, pgid).await;
            Outcome::TimedOut
        }
    };
    // Make sure no stray grandchild keeps the pipes open after the job is done.
    if let Some(pg) = pgid {
        let _ = killpg(pg, Signal::SIGKILL);
    }
    let _ = pump_out.await;
    let _ = pump_err.await;

    let duration = start.elapsed();
    let _ = events.send(JobEvent::Finished {
        job: spec.name.clone(),
        outcome,
        duration,
    });
    Ok(RunRecord {
        job: spec.name.clone(),
        command,
        cwd: spec.cwd.clone(),
        outcome,
        duration_ms: duration.as_millis(),
        finished_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    })
}

/// SIGTERM the process group, then SIGKILL if it hasn't exited within the grace period.
async fn terminate(child: &mut Child, pgid: Option<Pid>) {
    if let Some(pg) = pgid {
        let _ = killpg(pg, Signal::SIGTERM);
    }
    if tokio::time::timeout(TERM_GRACE, child.wait())
        .await
        .is_err()
    {
        if let Some(pg) = pgid {
            let _ = killpg(pg, Signal::SIGKILL);
        }
        let _ = child.wait().await;
    }
}

/// Forward a pipe to `tx` line by line. Invalid UTF-8 is replaced rather
/// than ending the stream (ISE in particular emits Latin-1). A carriage
/// return within a line (progress bars) keeps only the last segment.
async fn pump<R: AsyncRead + Unpin>(
    job: String,
    stream: Stream,
    r: R,
    tx: UnboundedSender<JobEvent>,
) {
    let mut reader = BufReader::new(r);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let line = String::from_utf8_lossy(&buf);
                let line = line.trim_end_matches(['\n', '\r']);
                let text = line.rsplit('\r').next().unwrap_or("").to_string();
                let _ = tx.send(JobEvent::Line {
                    job: job.clone(),
                    stream,
                    text,
                });
            }
        }
    }
}
