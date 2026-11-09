//! Fault injection. A fault fires from the profile's seed, its index and how
//! many times it has matched, so a replayed session injects the same faults.
//!
//! File faults act on SFTP traffic between an actor's sshfs mount and
//! `sftp-server` on the executor: a matching `open` never reaches the
//! executor and gets an error reply instead.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use needy_config::profile::{Chaos, Fault};
use needy_config::{glob_match, mix};

#[derive(Debug, Clone)]
pub struct Fired {
    pub errno: Option<String>,
    pub delay: Option<Duration>,
}

pub struct Injector {
    seed: u64,
    faults: Vec<Fault>,
    counts: Mutex<Vec<u64>>,
}

impl Injector {
    pub fn new(chaos: &Chaos) -> Self {
        Self {
            seed: chaos.seed,
            faults: chaos.faults.clone(),
            counts: Mutex::new(vec![0; chaos.faults.len()]),
        }
    }

    pub fn has(&self, on: &str) -> bool {
        self.faults.iter().any(|f| f.on == on)
    }

    pub fn faults(&self) -> &[Fault] {
        &self.faults
    }

    /// Counts a matching occurrence and reports the first fault that fires.
    pub fn check(&self, on: &str, subject: &str) -> Option<Fired> {
        let mut counts = self.counts.lock().unwrap();
        for (index, fault) in self.faults.iter().enumerate() {
            if fault.on != on {
                continue;
            }
            if let Some(pattern) = &fault.matcher {
                let matched = if on == "exec" {
                    command_match(pattern, subject)
                } else {
                    glob_match(pattern, subject)
                        || glob_match(pattern, subject.trim_start_matches('/'))
                };
                if !matched {
                    continue;
                }
            }
            counts[index] += 1;
            let n = counts[index];
            let fires = match (fault.nth, fault.probability) {
                (Some(nth), _) => n == nth,
                (None, Some(p)) => {
                    let roll = mix(self.seed, (index as u64) << 32 | n) as f64 / u64::MAX as f64;
                    roll < p
                }
                (None, None) => true,
            };
            if fires {
                return Some(Fired {
                    errno: fault.errno.clone(),
                    delay: fault.delay,
                });
            }
        }
        None
    }
}

/// Matches a command line word by word, like routing rules: `*` is one
/// argument, `**` the rest, and other words are globs. The first word also
/// matches the basename of a path.
fn command_match(pattern: &str, command: &str) -> bool {
    let words: Vec<&str> = command.split_whitespace().collect();
    let parts: Vec<&str> = pattern.split_whitespace().collect();
    fn go(parts: &[&str], words: &[&str], first: bool) -> bool {
        match parts.split_first() {
            None => words.is_empty(),
            Some((&"**", rest)) => (0..=words.len()).any(|i| go(rest, &words[i..], false)),
            Some((part, rest)) => match words.split_first() {
                None => false,
                Some((word, tail)) => {
                    let base = word.rsplit('/').next().unwrap_or(word);
                    let alternatives: Vec<&str> =
                        match part.strip_prefix('{').and_then(|p| p.strip_suffix('}')) {
                            Some(inner) => inner.split(',').collect(),
                            None => vec![part],
                        };
                    let hit = *part == "*"
                        || alternatives
                            .iter()
                            .any(|alt| glob_match(alt, word) || (first && glob_match(alt, base)));
                    hit && go(rest, tail, false)
                }
            },
        }
    }
    go(&parts, &words, true)
}

const SSH_FXP_OPEN: u8 = 3;
const SSH_FXP_STATUS: u8 = 101;

/// SFTP status code for an errno. sshfs maps unknown codes to EIO.
fn sftp_code(errno: &str) -> u32 {
    match errno {
        "ENOENT" => 2,
        "EACCES" => 3,
        "EPERM" => 4,
        "EOPNOTSUPP" | "ENOTSUP" => 8,
        _ => 255,
    }
}

fn read_u32(b: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(..4)?.try_into().ok()?))
}

/// Splits complete SFTP packets off the front of `buf`.
fn take_packets(buf: &mut Vec<u8>) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while let Some(len) = read_u32(buf) {
        let total = 4 + len as usize;
        if buf.len() < total {
            break;
        }
        out.push(buf.drain(..total).collect());
    }
    out
}

/// Builds `SSH_FXP_STATUS` for request `id`.
fn status_reply(id: u32, code: u32, message: &str) -> Vec<u8> {
    let mut body = vec![SSH_FXP_STATUS];
    body.extend(id.to_be_bytes());
    body.extend(code.to_be_bytes());
    body.extend((message.len() as u32).to_be_bytes());
    body.extend(message.as_bytes());
    body.extend(0u32.to_be_bytes()); // empty language tag
    let mut packet = (body.len() as u32).to_be_bytes().to_vec();
    packet.extend(body);
    packet
}

/// One mount's SFTP stream, framed in both directions.
pub struct SftpTap {
    to_server: Mutex<Vec<u8>>,
    to_client: Mutex<Vec<u8>>,
    /// Paths relative to the mount root are matched against faults.
    root: String,
}

pub struct Tapped {
    /// Bytes to forward to `sftp-server`.
    pub forward: Vec<u8>,
    /// Replies to send straight back to sshfs.
    pub replies: VecDeque<Vec<u8>>,
    /// Paths whose open failed, with the errno used.
    pub injected: Vec<(String, String)>,
}

impl SftpTap {
    pub fn new(root: &str) -> Self {
        Self {
            to_server: Mutex::new(Vec::new()),
            to_client: Mutex::new(Vec::new()),
            root: root.trim_end_matches('/').to_string(),
        }
    }

    /// Feeds bytes from sshfs; complete packets are forwarded or answered.
    pub fn client_bytes(&self, bytes: &[u8], injector: &Injector) -> Tapped {
        let mut buf = self.to_server.lock().unwrap();
        buf.extend_from_slice(bytes);
        let mut tapped = Tapped {
            forward: Vec::new(),
            replies: VecDeque::new(),
            injected: Vec::new(),
        };
        for packet in take_packets(&mut buf) {
            if packet.get(4) == Some(&SSH_FXP_OPEN)
                && let (Some(id), Some(name_len)) = (read_u32(&packet[5..]), read_u32(&packet[9..]))
                && let Some(name) = packet.get(13..13 + name_len as usize)
            {
                let path = String::from_utf8_lossy(name).to_string();
                let relative = path
                    .strip_prefix(&self.root)
                    .unwrap_or(&path)
                    .trim_start_matches('/')
                    .to_string();
                if let Some(fired) = injector.check("file.open", &relative) {
                    let errno = fired.errno.unwrap_or_else(|| "EIO".into());
                    tapped.replies.push_back(status_reply(
                        id,
                        sftp_code(&errno),
                        &format!("needy chaos: {errno}"),
                    ));
                    tapped.injected.push((relative, errno));
                    continue;
                }
            }
            tapped.forward.extend(packet);
        }
        tapped
    }

    /// Feeds bytes from `sftp-server`; returns only complete packets, so
    /// injected replies never land in the middle of one.
    pub fn server_bytes(&self, bytes: &[u8]) -> Vec<u8> {
        let mut buf = self.to_client.lock().unwrap();
        buf.extend_from_slice(bytes);
        take_packets(&mut buf).concat()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chaos(yaml: &str) -> Chaos {
        serde_yaml::from_str(yaml).unwrap()
    }

    fn open_packet(id: u32, path: &str) -> Vec<u8> {
        let mut body = vec![SSH_FXP_OPEN];
        body.extend(id.to_be_bytes());
        body.extend((path.len() as u32).to_be_bytes());
        body.extend(path.as_bytes());
        body.extend(1u32.to_be_bytes()); // pflags
        body.extend(0u32.to_be_bytes()); // attrs flags
        let mut p = (body.len() as u32).to_be_bytes().to_vec();
        p.extend(body);
        p
    }

    #[test]
    fn command_patterns_match_words() {
        assert!(command_match("curl **", "curl -s https://example.com/a"));
        assert!(command_match("curl *", "/usr/bin/curl https://x/y"));
        assert!(!command_match("curl *", "curl -s https://x/y"));
        assert!(command_match("{git,hg} push **", "git push origin main"));
        assert!(!command_match("git push **", "git pull"));
    }

    #[test]
    fn nth_fires_once() {
        let i = Injector::new(&chaos("faults: [{on: exec, match: 'curl *', nth: 2}]"));
        assert!(i.check("exec", "curl a").is_none());
        assert!(i.check("exec", "curl b").is_some());
        assert!(i.check("exec", "curl c").is_none());
        assert!(i.check("exec", "wget a").is_none());
    }

    #[test]
    fn probability_is_reproducible() {
        let yaml = "seed: 42\nfaults: [{on: net.connect, probability: 0.3}]";
        let run = || {
            let i = Injector::new(&chaos(yaml));
            (0..200)
                .map(|_| i.check("net.connect", "example.com").is_some())
                .collect::<Vec<_>>()
        };
        let a = run();
        assert_eq!(a, run());
        let hits = a.iter().filter(|x| **x).count();
        assert!((30..90).contains(&hits), "{hits}");
    }

    #[test]
    fn sftp_open_is_answered_with_an_error() {
        let i = Injector::new(&chaos(
            "faults: [{on: file.open, match: '**/*.json', errno: EIO}]",
        ));
        let tap = SftpTap::new("/home/agent/workspace/");
        let mut stream = open_packet(7, "/home/agent/workspace/src/a.txt");
        stream.extend(open_packet(8, "/home/agent/workspace/src/package.json"));
        // Split mid-packet to exercise framing.
        let (a, b) = stream.split_at(10);
        let first = tap.client_bytes(a, &i);
        assert!(first.forward.is_empty() && first.replies.is_empty());
        let second = tap.client_bytes(b, &i);
        assert_eq!(
            second.forward,
            open_packet(7, "/home/agent/workspace/src/a.txt")
        );
        assert_eq!(second.replies.len(), 1);
        let reply = &second.replies[0];
        assert_eq!(reply[4], SSH_FXP_STATUS);
        assert_eq!(read_u32(&reply[5..]), Some(8));
        assert_eq!(read_u32(&reply[9..]), Some(255));
        assert_eq!(
            second.injected,
            vec![("src/package.json".into(), "EIO".into())]
        );
    }

    #[test]
    fn server_output_is_released_whole() {
        let tap = SftpTap::new("/w");
        let packet = status_reply(1, 0, "ok");
        assert!(tap.server_bytes(&packet[..5]).is_empty());
        assert_eq!(tap.server_bytes(&packet[5..]), packet);
    }
}
