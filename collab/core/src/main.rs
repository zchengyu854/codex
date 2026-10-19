use codex_collab_core::{runtime::Runtime, secrets::Keychain, store::Store};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().ok_or_else(|| {
        anyhow::anyhow!("usage: codex-collab-core STATE_DIRECTORY [CODEX_EXECUTABLE]")
    })?);
    let binary = PathBuf::from(args.next().unwrap_or("codex".into()));
    let store = Arc::new(Store::open(&root)?);
    let recovered = store.recover()?;
    if recovered > 0 {
        eprintln!("{recovered} interrupted tasks require inspection; they will not be replayed");
    }
    let runtime = Runtime::new(store, Arc::new(Keychain), binary);
    let scheduler = runtime.start();
    let stdout = Arc::new(tokio::sync::Mutex::new(tokio::io::stdout()));
    let mut updates = runtime.updates.subscribe();
    let updates_stdout = stdout.clone();
    let notifications = tokio::spawn(async move {
        loop {
            let update = match updates.recv().await {
                Ok(v) => v,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    json!({"kind":"resync_required"})
                }
                Err(_) => break,
            };
            let mut out = updates_stdout.lock().await;
            if out
                .write_all(
                    format!("{}\n", json!({"method":"collab/update","params":update})).as_bytes(),
                )
                .await
                .is_err()
            {
                break;
            }
            let _ = out.flush().await;
        }
    });
    let mut requests = tokio::task::JoinSet::new();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    loop {
        let line = tokio::select! {_=tokio::signal::ctrl_c()=>break,line=lines.next_line()=>line?};
        let Some(line) = line else { break };
        let value: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                let mut out = stdout.lock().await;
                out.write_all(b"{\"id\":null,\"error\":{\"message\":\"invalid JSON\"}}\n")
                    .await?;
                out.flush().await?;
                continue;
            }
        };
        let rt = runtime.clone();
        let stdout = stdout.clone();
        requests.spawn(async move {
            let id = value.get("id").cloned().unwrap_or(Value::Null);
            let method = value["method"].as_str().unwrap_or("");
            let args = value.get("params").cloned().unwrap_or(json!({}));
            let response = match rt.dispatch(method, args).await {
                Ok(v) => json!({"id":id,"result":v}),
                Err(e) => json!({"id":id,"error":{"message":e.to_string()}}),
            };
            let mut out = stdout.lock().await;
            let _ = out.write_all(format!("{response}\n").as_bytes()).await;
            let _ = out.flush().await;
        });
    }
    while requests.join_next().await.is_some() {}
    runtime.stop().await;
    scheduler.await?;
    notifications.abort();
    Ok(())
}
