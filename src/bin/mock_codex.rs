//! Test double for `codex exec --json`.

use anyhow::Result;
use serde_json::json;
use std::process::{Command, Stdio};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if !args.iter().any(|a| a == "exec") {
        eprintln!("mock-codex only supports `exec`");
        std::process::exit(2);
    }

    println!(
        "{}",
        json!({"type":"thread.started","thread_id":"mock-thread"})
    );
    println!("{}", json!({"type":"turn.started"}));

    if let Ok(cmds) = std::env::var("MOCK_CODEX_RUN") {
        for cmd in cmds.split(";;").filter(|c| !c.trim().is_empty()) {
            println!(
                "{}",
                json!({"type":"item.started","item":{"type":"command_execution","command":cmd}})
            );
            let ok = if cfg!(windows) {
                Command::new("cmd.exe")
                    .arg("/C")
                    .arg(cmd)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()?
                    .success()
            } else {
                Command::new("sh")
                    .arg("-c")
                    .arg(cmd)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()?
                    .success()
            };
            println!(
                "{}",
                json!({"type":"item.completed","item":{"type":"command_execution","command":cmd,"status":if ok {"completed"} else {"failed"}}})
            );
        }
    }

    println!(
        "{}",
        json!({"type":"item.completed","item":{"type":"agent_message","text":"mock codex done"}})
    );
    println!(
        "{}",
        json!({"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":5}})
    );
    Ok(())
}
