use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::time::Instant;

/// Writer for asciicast v2 format files (`.cast`).
///
/// Produces JSONL files compatible with `asciinema play` and the
/// `asciinema-player` web component. Header is written on creation;
/// each `write_output` call appends a timestamped `"o"` event.
pub struct AsciicastWriter {
    writer: BufWriter<std::fs::File>,
    start: Instant,
}

impl AsciicastWriter {
    /// Create a new `.cast` file at `path` and write the v2 header.
    pub fn create(path: &Path, width: u16, height: u16, title: &str) -> io::Result<Self> {
        let file = std::fs::File::create(path)?;
        let mut writer = BufWriter::new(file);

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let header = serde_json::json!({
            "version": 2,
            "width": width,
            "height": height,
            "timestamp": timestamp,
            "title": title,
            "env": { "TERM": "xterm-256color" }
        });
        serde_json::to_writer(&mut writer, &header)?;
        writer.write_all(b"\n")?;
        writer.flush()?;

        Ok(Self {
            writer,
            start: Instant::now(),
        })
    }

    /// Append an output (`"o"`) event with the elapsed time since recording started.
    pub fn write_output(&mut self, data: &[u8]) -> io::Result<()> {
        let elapsed = self.start.elapsed().as_secs_f64();
        let text = String::from_utf8_lossy(data);
        let event = serde_json::json!([elapsed, "o", text]);
        serde_json::to_writer(&mut self.writer, &event)?;
        self.writer.write_all(b"\n")?;
        Ok(())
    }

    /// Flush and close the writer.
    pub fn finish(mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

/// Convert a plain log file to an asciicast v2 file with approximate timing.
///
/// Each line is spaced 50ms apart to give a rough playback feel.
pub fn convert_log_to_cast(
    log_path: &Path,
    cast_path: &Path,
    width: u16,
    height: u16,
    title: &str,
) -> io::Result<()> {
    let log_content = std::fs::read(log_path)?;
    if log_content.is_empty() {
        return Ok(());
    }

    let file = std::fs::File::create(cast_path)?;
    let mut writer = BufWriter::new(file);

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let header = serde_json::json!({
        "version": 2,
        "width": width,
        "height": height,
        "timestamp": timestamp,
        "title": title,
        "env": { "TERM": "xterm-256color" }
    });
    serde_json::to_writer(&mut writer, &header)?;
    writer.write_all(b"\n")?;

    // Split on newlines and emit each line as an event spaced 50ms apart.
    let mut elapsed = 0.0_f64;
    let step = 0.05;
    for chunk in log_content.split(|&b| b == b'\n') {
        // Re-append the newline that split consumed.
        let mut line = chunk.to_vec();
        line.push(b'\n');
        let text = String::from_utf8_lossy(&line);
        let event = serde_json::json!([elapsed, "o", text]);
        serde_json::to_writer(&mut writer, &event)?;
        writer.write_all(b"\n")?;
        elapsed += step;
    }

    writer.flush()?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn writes_valid_header_and_events() {
        let dir = tempfile::tempdir().unwrap();
        let cast_path = dir.path().join("test.cast");

        let mut writer = AsciicastWriter::create(&cast_path, 80, 24, "test recording").unwrap();
        writer.write_output(b"hello world\r\n").unwrap();
        writer.write_output(b"line two\r\n").unwrap();
        writer.finish().unwrap();

        let content = std::fs::read_to_string(&cast_path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 3, "header + 2 events");

        let header: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(header["version"], 2);
        assert_eq!(header["width"], 80);
        assert_eq!(header["height"], 24);
        assert_eq!(header["title"], "test recording");

        let event: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert!(event[0].as_f64().unwrap() >= 0.0);
        assert_eq!(event[1], "o");
        assert_eq!(event[2], "hello world\r\n");
    }

    #[test]
    fn convert_log_produces_valid_cast() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("test.log");
        let cast_path = dir.path().join("test.cast");

        std::fs::write(&log_path, "first line\nsecond line\n").unwrap();
        convert_log_to_cast(&log_path, &cast_path, 120, 40, "tmux replay").unwrap();

        let content = std::fs::read_to_string(&cast_path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert!(lines.len() >= 2, "header + at least one event");

        let header: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(header["version"], 2);
        assert_eq!(header["title"], "tmux replay");

        // Second event should have elapsed > 0
        if lines.len() > 2 {
            let event: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
            assert!(event[0].as_f64().unwrap() > 0.0);
        }
    }

    #[test]
    fn convert_empty_log_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("empty.log");
        let cast_path = dir.path().join("empty.cast");

        std::fs::write(&log_path, "").unwrap();
        convert_log_to_cast(&log_path, &cast_path, 80, 24, "empty").unwrap();

        assert!(!cast_path.exists(), "empty log should not produce a cast file");
    }
}
