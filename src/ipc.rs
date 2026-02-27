use std::{
	collections::hash_map::DefaultHasher,
	hash::{Hash as _, Hasher as _},
	path::{Path, PathBuf}
};

use flume::{Receiver, Sender};
use tokio::{
	io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader},
	net::UnixListener
};

use crate::{
	converter::ConverterMsg,
	renderer::RenderNotif,
	synctex
};

/// Compute the deterministic socket path for a given PDF file.
#[must_use]
pub fn socket_path(pdf_path: &Path) -> PathBuf {
	let mut hasher = DefaultHasher::new();
	pdf_path.hash(&mut hasher);
	let hash = hasher.finish();
	PathBuf::from(format!("/tmp/pdftui-{hash:016x}.sock"))
}

/// Remove the socket file on exit.
pub fn cleanup_socket(path: &Path) {
	let _ = std::fs::remove_file(path);
}

/// Start the IPC listener. Returns the socket path and a receiver that yields page numbers
/// whenever a successful forward search occurs.
#[must_use]
pub fn start_ipc_listener(
	pdf_path: &Path,
	to_renderer: Sender<RenderNotif>,
	to_converter: Sender<ConverterMsg>
) -> (PathBuf, Receiver<usize>) {
	let sock = socket_path(pdf_path);

	// Clean up stale socket from previous run
	let _ = std::fs::remove_file(&sock);

	let listener = UnixListener::bind(&sock).expect("cannot bind IPC socket");

	let (page_tx, page_rx) = flume::unbounded();
	let pdf = pdf_path.to_path_buf();

	tokio::spawn(async move {
		loop {
			let Ok((stream, _addr)) = listener.accept().await else {
				break;
			};

			let to_renderer = to_renderer.clone();
			let to_converter = to_converter.clone();
			let page_tx = page_tx.clone();
			let pdf = pdf.clone();

			tokio::spawn(async move {
				let (reader, mut writer) = stream.into_split();
				let mut lines = BufReader::new(reader).lines();

				while let Ok(Some(line)) = lines.next_line().await {
					let response = handle_ipc_line(
						&line,
						&pdf,
						&to_renderer,
						&to_converter,
						&page_tx
					);
					let msg = match response {
						Ok(page) => format!("ok {page}\n"),
						Err(e) => format!("error {e}\n")
					};
					if writer.write_all(msg.as_bytes()).await.is_err() {
						break;
					}
				}
			});
		}
	});

	(sock, page_rx)
}

/// Connect to a running pdftui instance and send a forward search command.
/// Returns the response string (e.g. "ok 3" or "error ...").
pub async fn send_forward(pdf_path: &Path, line: u32, col: u32, file: &str) -> Result<String, String> {
	let sock = socket_path(pdf_path);
	let stream = tokio::net::UnixStream::connect(&sock)
		.await
		.map_err(|e| format!("cannot connect to {}: {e}", sock.display()))?;

	let (reader, mut writer) = stream.into_split();

	let cmd = format!("forward {line} {col} {file}\n");
	writer
		.write_all(cmd.as_bytes())
		.await
		.map_err(|e| format!("write error: {e}"))?;
	writer
		.shutdown()
		.await
		.map_err(|e| format!("shutdown error: {e}"))?;

	let mut lines = BufReader::new(reader).lines();
	let response = lines
		.next_line()
		.await
		.map_err(|e| format!("read error: {e}"))?
		.unwrap_or_default();

	Ok(response.trim().to_string())
}

fn handle_ipc_line(
	line: &str,
	pdf_path: &Path,
	to_renderer: &Sender<RenderNotif>,
	to_converter: &Sender<ConverterMsg>,
	page_tx: &Sender<usize>
) -> Result<usize, String> {
	let parts: Vec<&str> = line.trim().splitn(4, ' ').collect();
	if parts.is_empty() {
		return Err("empty command".into());
	}

	match parts[0] {
		"forward" => {
			if parts.len() < 4 {
				return Err("usage: forward <line> <col> <file>".into());
			}
			let line_num: usize = parts[1].parse().map_err(|e| format!("bad line: {e}"))?;
			let col: usize = parts[2].parse().map_err(|e| format!("bad col: {e}"))?;
			let file = parts[3];

			let result = synctex::forward_search(line_num, col, file, pdf_path)
				.map_err(|e| e.to_string())?;

			let _ = to_renderer.send(RenderNotif::SyncTexJump {
				page: result.page,
				h: result.h,
				v: result.v,
				width: result.width,
				height: result.height
			});
			let _ = to_converter.send(ConverterMsg::GoToPage(result.page));
			let _ = page_tx.send(result.page);

			Ok(result.page)
		}
		other => Err(format!("unknown command: {other}"))
	}
}
