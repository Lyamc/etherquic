//! How this exe was started: client, host service, or a confirmation signal.

#![forbid(unsafe_code)]

use crate::config;

pub const USAGE: &str = "\
etherquic-host
    Run as the Windows service (the service manager starts it this way).
etherquic-host --console
    Same host loop in the foreground.
etherquic-host --client --peer MAC [--nic NAME] [--service PORT]
    Connect to a host. This does not install a service.
etherquic-host --approve
    Let the waiting player connect. One player, then the file is consumed.
etherquic-host --deny
    Refuse the waiting player.
etherquic-host --desk
    Capture the console desktop for the service. The service starts this.

A service installed without --unattended waits for --approve.
A service installed with --unattended accepts the player immediately.
";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    Help,
    Service,
    Console,
    Client {
        peer: [u8; 6],
        nic: Option<String>,
        service: Option<u16>,
    },
    Approve,
    Deny,
    Desk,
}

pub fn parse<I, S>(args: I) -> Result<Mode, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let args: Vec<String> = args.into_iter().map(|s| s.as_ref().to_string()).collect();
    if args
        .iter()
        .any(|arg| arg == "--help" || arg == "-h" || arg == "/?")
    {
        return Ok(Mode::Help);
    }
    let mut console = false;
    let mut client = false;
    let mut approve = false;
    let mut deny = false;
    let mut desk = false;
    let mut peer = None;
    let mut nic = None;
    let mut service = None;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--console" {
            console = true;
        } else if arg == "--client" {
            client = true;
        } else if arg == "--approve" {
            approve = true;
        } else if arg == "--deny" {
            deny = true;
        } else if arg == "--desk" {
            desk = true;
        } else if let Some(value) = takes_value(arg, &args, &mut i, "--peer")? {
            peer = Some(config::parse_mac(&value).ok_or_else(|| format!("invalid mac {value}"))?);
        } else if let Some(value) = takes_value(arg, &args, &mut i, "--nic")? {
            if value.is_empty() {
                return Err("--nic needs an adapter name".into());
            }
            nic = Some(value);
        } else if let Some(value) = takes_value(arg, &args, &mut i, "--service")? {
            service = Some(
                value
                    .parse::<u16>()
                    .map_err(|_| format!("invalid service {value}"))?,
            );
        } else {
            return Err(format!("unknown argument {arg}"));
        }
        i += 1;
    }
    let signals = [approve, deny, client].iter().filter(|flag| **flag).count();
    if signals > 1 {
        return Err("choose one of --client, --approve, or --deny".into());
    }
    if (approve || deny) && (console || nic.is_some() || peer.is_some() || service.is_some()) {
        return Err("--approve and --deny take no other arguments".into());
    }
    if desk && (console || client || approve || deny || peer.is_some() || nic.is_some() || service.is_some())
    {
        return Err("--desk takes no other arguments".into());
    }
    if desk {
        return Ok(Mode::Desk);
    }
    if approve {
        return Ok(Mode::Approve);
    }
    if deny {
        return Ok(Mode::Deny);
    }
    if client {
        let Some(peer) = peer else {
            return Err("--client needs --peer MAC".into());
        };
        return Ok(Mode::Client { peer, nic, service });
    }
    if peer.is_some() || service.is_some() || nic.is_some() {
        return Err("--peer, --nic, and --service are for --client".into());
    }
    if console {
        Ok(Mode::Console)
    } else if args.is_empty() {
        Ok(Mode::Service)
    } else {
        Err("unknown arguments".into())
    }
}

fn takes_value(
    arg: &str,
    args: &[String],
    index: &mut usize,
    flag: &str,
) -> Result<Option<String>, String> {
    let equals = format!("{flag}=");
    if let Some(value) = arg.strip_prefix(&equals) {
        if value.is_empty() {
            return Err(format!("{flag} needs a value"));
        }
        return Ok(Some(value.to_string()));
    }
    if arg != flag {
        return Ok(None);
    }
    let next = args
        .get(*index + 1)
        .ok_or_else(|| format!("{flag} needs a value"))?;
    if next.starts_with('-') {
        return Err(format!("{flag} needs a value"));
    }
    *index += 1;
    Ok(Some(next.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_modes() {
        assert_eq!(parse(None::<&str>).unwrap(), Mode::Service);
        assert_eq!(parse(["--console"]).unwrap(), Mode::Console);
        match parse(["--client", "--peer", "02:11:22:33:44:55", "--nic", "Killer"]).unwrap() {
            Mode::Client { peer, nic, service } => {
                assert_eq!(peer, [0x02, 0x11, 0x22, 0x33, 0x44, 0x55]);
                assert_eq!(nic.as_deref(), Some("Killer"));
                assert_eq!(service, None);
            }
            other => panic!("expected client, got {other:?}"),
        }
        assert_eq!(parse(["--approve"]).unwrap(), Mode::Approve);
        assert_eq!(parse(["--deny"]).unwrap(), Mode::Deny);
        assert!(parse(["--client"]).is_err());
        assert!(parse(["--approve", "--client", "--peer", "02:11:22:33:44:55"]).is_err());
        assert_eq!(parse(["--help"]).unwrap(), Mode::Help);
        assert_eq!(parse(["--desk"]).unwrap(), Mode::Desk);
        assert!(parse(["--desk", "--console"]).is_err());
    }
}
