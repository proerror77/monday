// Offline protocol peer. It never invokes a model, network or cloud backend.
use std::io::{self, BufRead, Write};
fn field(line: &str, key: &str) -> String {
    line.split(&format!("\"{key}\":\"")).nth(1).unwrap().split('"').next().unwrap().into()
}
fn main() {
    let home = std::path::PathBuf::from(std::env::var("CODEX_HOME").unwrap());
    let workspace = std::env::current_dir().unwrap();
    let mut intent = String::new();
    let mut text = String::new();
    for line in io::stdin().lock().lines() {
        let line = line.unwrap();
        if !line.contains("\"id\":") || !line.contains("\"method\":") { continue; }
        let id = line.split("\"id\":").nth(1).unwrap().split([',','}']).next().unwrap();
        let method = field(&line, "method");
        let result = match method.as_str() {
            "initialize" => "{}".to_string(),
            "thread/start" | "thread/resume" => {
                assert!(!line.contains("danger-full-access"));
                let dir = home.join("sessions");
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join("rollout-thread-fixture.jsonl"), "native thread-fixture\n").unwrap();
                format!(r#"{{"thread":{{"id":"thread-fixture"}},"approvalPolicy":"untrusted","sandbox":{{"type":"readOnly","networkAccess":false}},"cwd":"{}"}}"#, workspace.display())
            }
            "turn/start" => {
                intent = field(&line,"clientUserMessageId");
                text = field(&line,"text");
                std::fs::write(home.join("received-intent"), &intent).unwrap();
                std::fs::write(home.join("received-text"), &text).unwrap();
                if workspace.join("disconnect").exists() { return; }
                if workspace.join("oversize").exists() {
                    println!("{}", "x".repeat(1024*1024+1));
                    io::stdout().flush().unwrap();
                    continue;
                }
                println!(r#"{{"id":700,"method":"item/commandExecution/requestApproval","params":{{"threadId":"thread-fixture","command":"blocked"}}}}"#);
                r#"{"turn":{"id":"turn-fixture"}}"#.to_string()
            }
            "thread/read" => {
                if intent.is_empty() { intent=std::fs::read_to_string(home.join("received-intent")).unwrap_or_default(); }
                if text.is_empty() { text=std::fs::read_to_string(home.join("received-text")).unwrap_or_default(); }
                format!(r#"{{"thread":{{"id":"thread-fixture","turns":[{{"id":"turn-fixture","items":[{{"type":"userMessage","clientId":"{intent}","content":[{{"type":"text","text":"{text}"}}]}}]}}]}}}}"#)
            }
            "thread/items/list" => {
                if intent.is_empty() { intent=std::fs::read_to_string(home.join("received-intent")).unwrap_or_default(); }
                if text.is_empty() { text=std::fs::read_to_string(home.join("received-text")).unwrap_or_default(); }
                format!(r#"{{"data":[{{"turnId":"turn-fixture","item":{{"type":"userMessage","clientId":"{intent}","content":[{{"type":"text","text":"{text}"}}]}}}}],"nextCursor":null}}"#)
            }
            "turn/interrupt" => "{}".into(),
            _ => panic!("unexpected test request"),
        };
        println!("{{\"id\":{id},\"result\":{result}}}");
        io::stdout().flush().unwrap();
        if method == "turn/start" && workspace.join("fragment").exists() {
            print!("{{\"me");
            io::stdout().flush().unwrap();
            std::fs::write(workspace.join("fragment.sent"), "").unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !workspace.join("fragment.release").exists() {
                assert!(std::time::Instant::now() < deadline, "fragment fixture was not released");
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            println!("thod\":\"thread/status/changed\",\"params\":{{\"threadId\":\"thread-fixture\"}}}}");
            io::stdout().flush().unwrap();
        }
    }
}
