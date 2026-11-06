use anyhow::{Context, Result, ensure};
use reqwest::{Client, Method, Response};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Clone, Serialize, Deserialize)]
pub struct Profile {
    pub url: String,
    pub token: String,
    pub client_id: String,
    #[serde(default)]
    pub ca_cert: Option<PathBuf>,
}

pub fn transport(url: &str, ca_cert: Option<&Path>) -> Result<Client> {
    let url = reqwest::Url::parse(url)?;
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "URL cannot contain credentials, query or fragment"
    );
    let loopback = url.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    ensure!(
        url.scheme() == "https" || (url.scheme() == "http" && loopback),
        "unencrypted HTTP is allowed only on loopback; use HTTPS or SSH forwarding"
    );
    let mut builder = Client::builder()
        .http1_only()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10));
    if let Some(path) = ca_cert {
        builder =
            builder.add_root_certificate(reqwest::Certificate::from_pem(&std::fs::read(path)?)?);
    }
    Ok(builder.build()?)
}

pub struct Remote {
    pub profile: Profile,
    pub http: Client,
}
impl Remote {
    pub fn open(path: &Path) -> Result<Self> {
        let profile: Profile = serde_json::from_slice(&crate::workspace::read_private(path)?)
            .context("invalid client profile")?;
        let http = transport(&profile.url, profile.ca_cert.as_deref())?;
        Ok(Self { profile, http })
    }
    pub async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<Response> {
        let mut request = self
            .http
            .request(
                method,
                format!("{}{path}", self.profile.url.trim_end_matches('/')),
            )
            .bearer_auth(&self.profile.token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await?;
        check(response).await
    }
    pub async fn connect(
        &self,
        path: &str,
        protocol: &str,
        request: &impl Serialize,
    ) -> Result<reqwest::Upgraded> {
        let response = self
            .http
            .post(format!("{}{path}", self.profile.url.trim_end_matches('/')))
            .bearer_auth(&self.profile.token)
            .header("connection", "Upgrade")
            .header("upgrade", protocol)
            .json(request)
            .send()
            .await?;
        ensure!(
            response.status() == reqwest::StatusCode::SWITCHING_PROTOCOLS,
            "remote tunnel rejected with HTTP {}",
            response.status()
        );
        ensure!(
            response
                .headers()
                .get("upgrade")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.eq_ignore_ascii_case(protocol)),
            "unexpected tunnel protocol"
        );
        Ok(response.upgrade().await?)
    }
}
pub async fn check(response: Response) -> Result<Response> {
    ensure!(
        response.status().is_success(),
        "remote operation rejected with HTTP {}",
        response.status()
    );
    Ok(response)
}

pub struct Terminal {
    original: libc::termios,
}
impl Terminal {
    pub fn save() -> Result<Self> {
        let mut original = unsafe { std::mem::zeroed() };
        ensure!(
            unsafe { libc::tcgetattr(0, &mut original) } == 0,
            "cannot read terminal settings"
        );
        Ok(Self { original })
    }
    fn raw() -> Result<Self> {
        let terminal = Self::save()?;
        let original = terminal.original;
        let mut raw = original;
        unsafe {
            libc::cfmakeraw(&mut raw);
        }
        ensure!(
            unsafe { libc::tcsetattr(0, libc::TCSANOW, &raw) } == 0,
            "cannot set terminal raw mode"
        );
        Ok(terminal)
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &self.original);
        }
    }
}

pub fn window() -> Result<crate::process::Window> {
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    ensure!(
        unsafe { libc::ioctl(0, libc::TIOCGWINSZ as _, &mut size) } == 0,
        "cannot read terminal size"
    );
    Ok(crate::process::Window {
        rows: size.ws_row.max(1),
        cols: size.ws_col.max(1),
    })
}

pub async fn tunnel(socket: reqwest::Upgraded, pty: bool) -> Result<crate::process::Exit> {
    let _terminal = if pty && unsafe { libc::isatty(0) } == 1 {
        Some(Terminal::raw()?)
    } else {
        None
    };
    let (mut reader, mut writer) = tokio::io::split(socket);
    let (sender, mut receiver) = tokio::sync::mpsc::channel::<(u8, Vec<u8>)>(32);
    let input = sender.clone();
    let mut stdin = tokio::spawn(async move {
        let mut stdin = tokio::io::stdin();
        let mut bytes = vec![0; 32768];
        loop {
            let n = stdin.read(&mut bytes).await?;
            if n == 0 {
                input.send((crate::wire::EOF, Vec::new())).await?;
                return Ok::<_, anyhow::Error>(());
            }
            input
                .send((crate::wire::STDIN, bytes[..n].to_vec()))
                .await?;
        }
    });
    let signal_sender = sender.clone();
    let mut signal_task = tokio::spawn(async move {
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
        let mut resize =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())?;
        loop {
            let signal = tokio::select! {_=interrupt.recv()=>libc::SIGINT,_=term.recv()=>libc::SIGTERM,_=hangup.recv()=>libc::SIGHUP,_=resize.recv()=>libc::SIGWINCH};
            if signal == libc::SIGWINCH {
                if pty {
                    signal_sender
                        .send((crate::wire::RESIZE, serde_json::to_vec(&window()?)?))
                        .await?;
                }
            } else {
                signal_sender
                    .send((crate::wire::SIGNAL, serde_json::to_vec(&signal)?))
                    .await?;
            }
        }
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    });
    drop(sender);
    let mut write_task = tokio::spawn(async move {
        while let Some((kind, bytes)) = receiver.recv().await {
            crate::wire::write(&mut writer, kind, &bytes).await?;
        }
        Ok::<_, anyhow::Error>(())
    });
    let output = async {
        let mut stdout = tokio::io::stdout();
        let mut stderr = tokio::io::stderr();
        loop {
            let (kind, bytes) = crate::wire::read(&mut reader).await?;
            match kind {
                crate::wire::STDOUT => {
                    stdout.write_all(&bytes).await?;
                    stdout.flush().await?;
                }
                crate::wire::STDERR => {
                    stderr.write_all(&bytes).await?;
                    stderr.flush().await?;
                }
                crate::wire::EXIT => {
                    let exit: crate::process::Exit = serde_json::from_slice(&bytes)?;
                    ensure!((0..=255).contains(&exit.code), "invalid remote exit code");
                    return Ok::<_, anyhow::Error>(exit);
                }
                crate::wire::FAILURE => anyhow::bail!("remote process or transport failed"),
                _ => anyhow::bail!("invalid server frame"),
            }
        }
    };
    tokio::pin!(output);
    let mut input_done = false;
    let result = loop {
        tokio::select! {
            biased;
            result=&mut output=>break result,
            result=&mut write_task=>break result.map_err(anyhow::Error::from).and_then(|result|result).and_then(|_|Err(anyhow::anyhow!("tunnel input ended unexpectedly"))),
            result=&mut signal_task=>break result.map_err(anyhow::Error::from).and_then(|result|result).and_then(|_|Err(anyhow::anyhow!("signal handler ended unexpectedly"))),
            result=&mut stdin, if !input_done=>{
                input_done=true;
                if let Err(error)=result.map_err(anyhow::Error::from).and_then(|result|result){break Err(error);}
            }
        }
    };
    stdin.abort();
    signal_task.abort();
    write_task.abort();
    result
}
