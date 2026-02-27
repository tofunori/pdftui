use std::{
	path::Path,
	process::Command
};

/// Result of a forward SyncTeX lookup (source → PDF position)
#[derive(Debug)]
pub struct ForwardResult {
	/// 0-based page number
	pub page: usize,
	/// x coordinate in PDF points (from top-left)
	pub x: f32,
	/// y coordinate in PDF points (from top-left)
	pub y: f32,
	/// horizontal origin of enclosing box
	pub h: f32,
	/// vertical origin of enclosing box
	pub v: f32,
	/// width of enclosing box
	pub width: f32,
	/// height of enclosing box
	pub height: f32
}

/// Result of an inverse SyncTeX lookup (PDF position → source)
#[derive(Debug)]
pub struct InverseResult {
	pub input: String,
	pub line: usize,
	pub column: i32
}

#[derive(Debug)]
pub enum SyncTexError {
	/// The synctex CLI binary was not found
	NotFound,
	/// The synctex command failed
	CommandFailed(String),
	/// Could not parse the output
	ParseError(String),
	/// No .synctex.gz file found
	NoSyncTexFile
}

impl core::fmt::Display for SyncTexError {
	fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
		match self {
			Self::NotFound => write!(f, "synctex command not found"),
			Self::CommandFailed(e) => write!(f, "synctex command failed: {e}"),
			Self::ParseError(e) => write!(f, "could not parse synctex output: {e}"),
			Self::NoSyncTexFile => write!(f, "no .synctex.gz file found for this PDF")
		}
	}
}

/// Check if a .synctex.gz or .synctex file exists for the given PDF
#[must_use]
pub fn has_synctex_file(pdf_path: &Path) -> bool {
	let stem = pdf_path.with_extension("");
	let gz = stem.with_extension("synctex.gz");
	let plain = stem.with_extension("synctex");
	gz.exists() || plain.exists()
}

/// Forward search: given a source line, find the corresponding PDF position
///
/// `line` is 1-based, `column` is 0-based (pass 0 if unknown)
pub fn forward_search(
	line: usize,
	column: usize,
	input_file: &str,
	pdf_path: &Path
) -> Result<ForwardResult, SyncTexError> {
	let input_arg = format!("{line}:{column}:{input_file}");
	let pdf_str = pdf_path.to_str().ok_or_else(|| {
		SyncTexError::ParseError("PDF path is not valid UTF-8".into())
	})?;

	let output = Command::new("synctex")
		.args(["view", "-i", &input_arg, "-o", pdf_str])
		.output()
		.map_err(|e| {
			if e.kind() == std::io::ErrorKind::NotFound {
				SyncTexError::NotFound
			} else {
				SyncTexError::CommandFailed(e.to_string())
			}
		})?;

	if !output.status.success() {
		let stderr = String::from_utf8_lossy(&output.stderr);
		return Err(SyncTexError::CommandFailed(stderr.into_owned()));
	}

	let stdout = String::from_utf8_lossy(&output.stdout);
	parse_forward_output(&stdout)
}

/// Inverse search: given a PDF page and coordinates, find the source location
///
/// `page` is 1-based, `x` and `y` are in PDF points (72 dpi)
pub fn inverse_search(
	page: usize,
	x: f32,
	y: f32,
	pdf_path: &Path
) -> Result<InverseResult, SyncTexError> {
	let pdf_str = pdf_path.to_str().ok_or_else(|| {
		SyncTexError::ParseError("PDF path is not valid UTF-8".into())
	})?;
	let output_arg = format!("{page}:{x}:{y}:{pdf_str}");

	let output = Command::new("synctex")
		.args(["edit", "-o", &output_arg])
		.current_dir(pdf_path.parent().unwrap_or(Path::new(".")))
		.output()
		.map_err(|e| {
			if e.kind() == std::io::ErrorKind::NotFound {
				SyncTexError::NotFound
			} else {
				SyncTexError::CommandFailed(e.to_string())
			}
		})?;

	if !output.status.success() {
		let stderr = String::from_utf8_lossy(&output.stderr);
		return Err(SyncTexError::CommandFailed(stderr.into_owned()));
	}

	let stdout = String::from_utf8_lossy(&output.stdout);
	let mut result = parse_inverse_output(&stdout)?;

	// Canonicalize the input path: synctex may return a relative path.
	// Resolve it relative to the PDF's directory so nvr/nvim can find the buffer.
	let input_path = std::path::Path::new(&result.input);
	if input_path.is_relative() {
		if let Some(pdf_dir) = pdf_path.parent() {
			let absolute = pdf_dir.join(input_path);
			if let Ok(canonical) = absolute.canonicalize() {
				result.input = canonical.to_string_lossy().into_owned();
			} else {
				result.input = absolute.to_string_lossy().into_owned();
			}
		}
	}

	Ok(result)
}

fn parse_forward_output(output: &str) -> Result<ForwardResult, SyncTexError> {
	let mut page = None;
	let mut x = None;
	let mut y = None;
	let mut h = None;
	let mut v = None;
	let mut width = None;
	let mut height = None;
	let mut in_result = false;

	for line in output.lines() {
		let line = line.trim();
		if line == "SyncTeX result begin" {
			in_result = true;
			continue;
		}
		if line == "SyncTeX result end" {
			break;
		}
		if !in_result {
			continue;
		}

		if let Some((key, val)) = line.split_once(':') {
			match key {
				// Page is 1-based in synctex output, convert to 0-based
				"Page" => page = val.trim().parse::<usize>().ok().map(|p| p.saturating_sub(1)),
				"x" => x = val.trim().parse().ok(),
				"y" => y = val.trim().parse().ok(),
				"h" => h = val.trim().parse().ok(),
				"v" => v = val.trim().parse().ok(),
				"W" => width = val.trim().parse().ok(),
				"H" => height = val.trim().parse().ok(),
				_ => ()
			}
		}
	}

	match (page, x, y, h, v, width, height) {
		(Some(page), Some(x), Some(y), Some(h), Some(v), Some(width), Some(height)) =>
			Ok(ForwardResult { page, x, y, h, v, width, height }),
		_ => Err(SyncTexError::ParseError(
			"missing required fields in synctex view output".into()
		))
	}
}

fn parse_inverse_output(output: &str) -> Result<InverseResult, SyncTexError> {
	let mut input = None;
	let mut line = None;
	let mut column = None;
	let mut in_result = false;

	for text_line in output.lines() {
		let text_line = text_line.trim();
		if text_line == "SyncTeX result begin" {
			in_result = true;
			continue;
		}
		if text_line == "SyncTeX result end" {
			break;
		}
		if !in_result {
			continue;
		}

		if let Some((key, val)) = text_line.split_once(':') {
			match key {
				"Input" => input = Some(val.trim().to_string()),
				"Line" => line = val.trim().parse().ok(),
				"Column" => column = val.trim().parse().ok(),
				_ => ()
			}
		}
	}

	match (input, line) {
		(Some(input), Some(line)) => Ok(InverseResult {
			input,
			line,
			column: column.unwrap_or(-1)
		}),
		_ => Err(SyncTexError::ParseError(
			"missing required fields in synctex edit output".into()
		))
	}
}
