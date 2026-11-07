//! Helpers for tests: a one-shot fake hub on a local port.

/// One HTTP exchange on a local port: returns the hub URL and, once a
/// request came, its head and body.
pub fn fake_hub(status: &'static str, body: &'static str) -> (String, std::thread::JoinHandle<(String, String)>) {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream);
        let (mut head, mut len) = (String::new(), 0);
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                len = v.trim().parse().unwrap();
            }
            if line == "\r\n" {
                break;
            }
            head.push_str(&line);
        }
        let mut got = vec![0; len];
        reader.read_exact(&mut got).unwrap();
        let reply = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        reader.get_mut().write_all(reply.as_bytes()).unwrap();
        (head, String::from_utf8(got).unwrap())
    });
    (url, handle)
}
