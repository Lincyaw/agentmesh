use crate::protocol::{MAX_FRAME, Request, Response};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::path::Path;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

pub async fn read_frame(reader: impl AsyncRead + Unpin) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    BufReader::new(reader)
        .take((MAX_FRAME + 1) as u64)
        .read_until(b'\n', &mut data)
        .await?;
    ensure!(
        data.len() <= MAX_FRAME && data.last() == Some(&b'\n'),
        "invalid or oversized frame"
    );
    Ok(data)
}

pub async fn call(dir: &Path, request: Request) -> Result<Value> {
    tokio::time::timeout(std::time::Duration::from_secs(25), async {
        let mut stream = UnixStream::connect(dir.join("daemon.sock"))
            .await
            .context("daemon unavailable; run agentmesh daemon run")?;
        let mut data = serde_json::to_vec(&request)?;
        ensure!(data.len() < MAX_FRAME, "request too large");
        data.push(b'\n');
        stream.write_all(&data).await?;
        let response: Response = serde_json::from_slice(&read_frame(stream).await?)?;
        response.into_result()
    })
    .await
    .context("local daemon request timed out")?
}
