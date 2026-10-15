//! Optional Recall client: this app never downloads or loads embedding models.
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    time::{Duration, Instant},
};

pub fn rank(collection: &str, query: &str, records: &[Value], limit: usize) -> Option<Vec<String>> {
    if records.is_empty() {
        return Some(Vec::new());
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(gtk::glib::user_runtime_dir);
    let socket = runtime.join("recall-for-omarchy.sock");
    rank_at(&socket, collection, query, records, limit)
}

fn rank_at(
    socket: &std::path::Path,
    collection: &str,
    query: &str,
    records: &[Value],
    limit: usize,
) -> Option<Vec<String>> {
    let deadline = Instant::now() + Duration::from_secs(6);
    let mut hits = Vec::new();
    for batch in records.chunks(32) {
        let mut bytes = serde_json::to_vec(
            &json!({"api":1,"op":"records.search","collection":collection,"query":query,"records":batch,"limit":32}),
        )
        .ok()?;
        if bytes.len() >= 1_048_576 {
            return None;
        }
        bytes.push(b'\n');
        let mut stream = UnixStream::connect(socket).ok()?;
        let timeout = deadline.checked_duration_since(Instant::now())?;
        stream.set_read_timeout(Some(timeout)).ok()?;
        stream.set_write_timeout(Some(timeout)).ok()?;
        stream.write_all(&bytes).ok()?;
        let mut line = Vec::new();
        loop {
            stream
                .set_read_timeout(Some(deadline.checked_duration_since(Instant::now())?))
                .ok()?;
            let mut chunk = [0u8; 8192];
            let count = stream.read(&mut chunk).ok()?;
            if count == 0 {
                return None;
            }
            line.extend_from_slice(&chunk[..count]);
            if line.len() > 1_048_576 {
                return None;
            }
            if let Some(end) = line.iter().position(|b| *b == b'\n') {
                line.truncate(end + 1);
                break;
            }
        }
        if line.len() > 1_048_576 || line.last() != Some(&b'\n') {
            return None;
        }
        let reply: Value = serde_json::from_slice(&line).ok()?;
        if reply["api"] != 1 || reply["meaning"] != true || reply.get("error").is_some() {
            return None;
        }
        let allowed: HashSet<_> = batch.iter().filter_map(|r| r["id"].as_str()).collect();
        for hit in reply["hits"].as_array()? {
            let id = hit["id"].as_str()?;
            let score = hit["score"].as_f64()?;
            if !allowed.contains(id) || !score.is_finite() {
                return None;
            }
            hits.push((id.to_owned(), score));
        }
    }
    hits.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut seen = HashSet::new();
    Some(
        hits.into_iter()
            .filter_map(|(id, _)| seen.insert(id.clone()).then_some(id))
            .take(limit)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixListener;
    #[test]
    fn optional_service_and_scoped_results() {
        let socket = std::env::temp_dir().join(format!(
            "recall-client-{}-{}.sock",
            std::process::id(),
            std::thread::current().name().unwrap_or("test").replace(':', "_")
        ));
        let records = vec![json!({"id":"current","text":"holiday"})];
        assert!(rank_at(&socket, "test", "travel", &records, 10).is_none());
        for reply in [
            json!({"api":1,"meaning":true,"hits":[{"id":"current","score":0.8}]}),
            json!({"api":1,"meaning":true,"hits":[{"id":"deleted","score":0.9}]}),
            json!({"api":2,"meaning":true,"hits":[]}),
        ] {
            let listener = UnixListener::bind(&socket).unwrap();
            let expected =
                (reply["api"] == 1 && reply["hits"][0]["id"] == "current").then(|| vec!["current".to_owned()]);
            let worker = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut request)
                    .unwrap();
                let request: Value = serde_json::from_str(&request).unwrap();
                assert_eq!(request["collection"], "test");
                writeln!(stream, "{reply}").unwrap();
            });
            assert_eq!(rank_at(&socket, "test", "travel", &records, 10), expected);
            worker.join().unwrap();
            std::fs::remove_file(&socket).unwrap();
        }
    }
}
