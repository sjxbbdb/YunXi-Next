//! Small JSONL action fixture used only by process-boundary tests.

use std::io::{self, BufRead, Write};
use std::thread;
use std::time::Duration;

use serde_json::Value;
use yunxi_protocol::{SkillActionOutcome, SkillActionRequest, SkillActionResponse};

fn main() {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let mut line = String::new();
    if io::stdin().lock().read_line(&mut line).is_err() {
        std::process::exit(2);
    }
    if arguments.iter().any(|argument| argument == "--bad-frame") {
        println!("not-json");
        return;
    }
    if arguments.iter().any(|argument| argument == "--huge-frame") {
        let mut stdout = io::stdout().lock();
        let _ = stdout.write_all(&vec![
            b'x';
            yunxi_protocol::MAX_SKILL_ACTION_FRAME_BYTES + 1
        ]);
        let _ = stdout.flush();
        return;
    }
    if arguments.iter().any(|argument| argument == "--huge-stderr") {
        let mut stderr = io::stderr().lock();
        let _ = stderr.write_all(&vec![
            b'e';
            yunxi_protocol::MAX_SKILL_ACTION_FRAME_BYTES + 1
        ]);
        let _ = stderr.flush();
        return;
    }
    if arguments.iter().any(|argument| argument == "--crash") {
        std::process::exit(42);
    }
    let request = match serde_json::from_str::<SkillActionRequest>(&line) {
        Ok(request) => request,
        Err(_) => std::process::exit(3),
    };
    let input = request.input();
    if let Some(milliseconds) = input.get("sleep_millis").and_then(Value::as_u64) {
        thread::sleep(Duration::from_millis(milliseconds));
    }
    let value = input.get("value").and_then(Value::as_str).unwrap_or("ok");
    let response = SkillActionResponse::new(SkillActionOutcome::Success, Some(0), value, "", false)
        .expect("fixture response");
    println!(
        "{}",
        serde_json::to_string(&response).expect("fixture JSON")
    );
}
