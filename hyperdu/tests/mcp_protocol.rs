use std::{
    io::{BufRead, BufReader, Read, Write},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

use serde_json::{json, Value};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn send_json(stdin: &mut ChildStdin, message: &Value) -> TestResult {
    serde_json::to_writer(&mut *stdin, message)?;
    stdin.write_all(b"\n")?;
    stdin.flush()?;
    Ok(())
}

fn read_response(reader: &mut BufReader<ChildStdout>, id: u64) -> TestResult<Value> {
    loop {
        let mut line = String::new();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            return Err(format!("MCP server exited before response id={id}").into());
        }
        let message: Value = serde_json::from_str(line.trim())?;
        if message.get("id").and_then(Value::as_u64) == Some(id) {
            return Ok(message);
        }
    }
}

fn stop_server(
    mut child: Child,
    stdin: ChildStdin,
    mut reader: BufReader<ChildStdout>,
) -> TestResult {
    drop(stdin);
    let mut trailing = String::new();
    reader.read_to_string(&mut trailing)?;
    for line in trailing.lines().filter(|line| !line.trim().is_empty()) {
        let _: Value = serde_json::from_str(line.trim())?;
    }
    let status = child.wait()?;
    assert!(status.success(), "MCP server exited with {status}");
    Ok(())
}

fn spawn_server() -> TestResult<(Child, ChildStdin, BufReader<ChildStdout>)> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_hyperdu"))
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdin = child.stdin.take().ok_or("MCP stdin was not piped")?;
    let stdout = child.stdout.take().ok_or("MCP stdout was not piped")?;
    Ok((child, stdin, BufReader::new(stdout)))
}

#[test]
fn mcp_help_exposes_stdio_subcommand_and_literal_path_escape() -> TestResult {
    let output = Command::new(env!("CARGO_BIN_EXE_hyperdu"))
        .args(["mcp", "--help"])
        .output()?;
    assert!(output.status.success(), "mcp --help failed: {output:?}");
    let help = String::from_utf8(output.stdout)?;
    assert!(
        help.contains("stdio"),
        "help must explain the MCP transport: {help}"
    );
    assert!(
        help.contains("./mcp"),
        "help must document the literal path escape: {help}"
    );
    Ok(())
}

#[test]
fn mcp_stdio_handshake_lists_tools_and_scans_a_path() -> TestResult {
    let dir = tempfile::tempdir()?;
    let child_dir = dir.path().join("payload");
    std::fs::create_dir_all(&child_dir)?;
    std::fs::write(child_dir.join("data.bin"), vec![0_u8; 512])?;

    let (child, mut stdin, mut reader) = spawn_server()?;
    send_json(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "hyperdu-cli-test", "version": "0" }
            }
        }),
    )?;
    let initialized = read_response(&mut reader, 1)?;
    assert_eq!(initialized["id"], 1);
    assert!(initialized["result"]["serverInfo"]["name"].is_string());

    send_json(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    )?;
    send_json(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} }),
    )?;
    let listed = read_response(&mut reader, 2)?;
    let mut names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .ok_or("tools/list did not return an array")?
        .iter()
        .map(|tool| tool["name"].as_str().ok_or("tool name was not a string"))
        .collect::<Result<_, _>>()?;
    names.sort_unstable();
    assert_eq!(names, ["find_reclaimable", "list_volumes", "scan_path"]);

    send_json(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "scan_path",
                "arguments": {
                    "path": dir.path().display().to_string(),
                    "top_n": 2,
                    "max_depth": 0
                }
            }
        }),
    )?;
    let called = read_response(&mut reader, 3)?;
    assert_ne!(called["result"]["isError"], Value::Bool(true));
    assert!(called["result"]["content"].is_array());

    stop_server(child, stdin, reader)
}

#[test]
fn a_directory_named_mcp_can_be_scanned_with_option_terminator() -> TestResult {
    let dir = tempfile::tempdir()?;
    std::fs::create_dir(dir.path().join("mcp"))?;
    std::fs::write(dir.path().join("mcp/data.bin"), [0_u8; 512])?;
    for args in [["--", "mcp"].as_slice(), ["./mcp"].as_slice()] {
        let output = Command::new(env!("CARGO_BIN_EXE_hyperdu"))
            .current_dir(dir.path())
            .args(args)
            .output()?;
        assert!(
            output.status.success(),
            "literal mcp scan failed: {output:?}"
        );
        assert!(String::from_utf8(output.stdout)?.contains("files=1"));
    }
    Ok(())
}

#[test]
fn scan_progress_precedes_result_and_keeps_the_request_token() -> TestResult {
    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join("data.bin"), [1u8; 512])?;
    let (child, mut stdin, mut reader) = spawn_server()?;
    send_json(
        &mut stdin,
        &json!({
            "jsonrpc":"2.0", "id":1, "method":"initialize", "params":{
                "protocolVersion":"2024-11-05", "capabilities":{},
                "clientInfo":{"name":"progress-test","version":"0"}
            }
        }),
    )?;
    read_response(&mut reader, 1)?;
    send_json(
        &mut stdin,
        &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    )?;
    for (id, token) in [(2u64, Some("scan-one")), (3, Some("scan-two")), (4, None)] {
        let mut params = json!({"name":"scan_path", "arguments":{
            "path":dir.path().display().to_string(),"top_n":1,"max_depth":0
        }});
        if let Some(token) = token {
            params["_meta"] = json!({"progressToken":token});
        }
        send_json(
            &mut stdin,
            &json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":params}),
        )?;
        let mut notifications = Vec::new();
        let response = loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                return Err("server exited during progress test".into());
            }
            let message: Value = serde_json::from_str(line.trim())?;
            if message.get("id").and_then(Value::as_u64) == Some(id) {
                break message;
            }
            if message["method"] == "notifications/progress" {
                notifications.push(message["params"].clone());
            }
        };
        assert!(response.get("error").is_none(), "{response}");
        assert_eq!(
            response["result"]["structuredContent"]["entries"][0]["files"],
            1
        );
        if let Some(token) = token {
            assert!(
                !notifications.is_empty(),
                "start must arrive before the result"
            );
            assert_eq!(notifications[0]["progress"], 0.0);
            assert!(notifications[0]["message"]
                .as_str()
                .unwrap()
                .contains("Scanning"));
            let mut previous = -1.0;
            for notification in notifications {
                assert_eq!(notification["progressToken"], token);
                assert!(
                    notification.get("total").is_none(),
                    "unknown totals must not invent a percentage"
                );
                let current = notification["progress"].as_f64().unwrap();
                assert!(current > previous, "progress must increase");
                previous = current;
            }
        } else {
            assert!(
                notifications.is_empty(),
                "no unsolicited progress without a token"
            );
        }
    }
    stop_server(child, stdin, reader)
}
