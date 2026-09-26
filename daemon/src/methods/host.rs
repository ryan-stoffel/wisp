//! `initialize`, `host/health`, and `host/version`.

use std::collections::BTreeMap;
use std::fs;

use tracing::info;
use wisp_protocol::framing::MAX_FRAME_BYTES;
use wisp_protocol::jsonrpc::{ErrorObject, Request};
use wisp_protocol::{
    Capabilities, ClientInfo, HostHealthParams, HostHealthResult, HostVersionParams,
    HostVersionResult, IncompatibleProtocolDetail, InitializeParams, InitializeProtocol,
    InitializeResult, ProtocolRange,
};

use super::Context;
use crate::VERSION;
use crate::logging::untrusted;
use crate::server::Daemon;

const SYSTEM_VERSION: &str = "/System/Library/CoreServices/SystemVersion.plist";

/// What `initialize` settled for a connection.
#[derive(Debug)]
pub(crate) struct Session {
    pub protocol: u32,
    pub client: ClientInfo,
}

/// `initialize`: agrees on a protocol version, or fails with `incompatibleProtocol`.
///
/// The client's range is read first, from the part of the params that never changes shape, so a
/// client from any version gets `incompatibleProtocol` rather than a params error.
pub(crate) fn initialize(
    daemon: &Daemon,
    request: &Request,
) -> Result<(Session, InitializeResult), ErrorObject> {
    let InitializeProtocol { protocol } = request.params()?;
    let Some(version) = ProtocolRange::SUPPORTED.highest_common(protocol) else {
        return Err(ErrorObject::incompatible_protocol(
            &IncompatibleProtocolDetail {
                requested: protocol,
                supported: ProtocolRange::SUPPORTED,
                wispd: VERSION.to_owned(),
            },
        ));
    };
    let InitializeParams {
        client,
        capabilities,
        ..
    } = request.params()?;
    let capability_names = capabilities.0.keys().cloned().collect::<Vec<_>>().join(",");
    info!(
        client = ?untrusted(&client.name),
        client_version = ?untrusted(&client.version),
        protocol = version,
        capabilities = ?untrusted(&capability_names),
        "initialized"
    );
    let result = InitializeResult {
        protocol: version,
        wispd: VERSION.to_owned(),
        log_id: daemon.log.id(),
        capabilities: capabilities_advertised(),
        max_frame_bytes: u64::try_from(MAX_FRAME_BYTES).unwrap_or(u64::MAX),
    };
    let session = Session {
        protocol: version,
        client,
    };
    Ok((session, result))
}

/// The capabilities this wispd advertises. M2 adds `accounts` (#117) and `agentClis` (#114),
/// distinct capabilities since the two features (stored API keys and detected CLIs) can ship
/// independently; M3 adds `agents` (#156): the `agent/*` methods and `agent.*` events, and
/// `agentReview` (#157): `agent/diff`, `agent/file`, `agent/accept`, `agent/requestChanges`, and
/// `agent.accepted`, so an editor can tell a host that reviews runs from one that only runs them.
fn capabilities_advertised() -> Capabilities {
    Capabilities(BTreeMap::from([
        ("accounts".to_owned(), serde_json::Map::new()),
        ("agentClis".to_owned(), serde_json::Map::new()),
        ("agentReview".to_owned(), serde_json::Map::new()),
        ("agents".to_owned(), serde_json::Map::new()),
    ]))
}

pub(crate) fn health(context: &Context, _: HostHealthParams) -> HostHealthResult {
    let daemon = &context.daemon;
    HostHealthResult {
        uptime_seconds: daemon.started.elapsed().as_secs(),
        store: daemon.store.state(),
        running_agents: daemon.agents.running(),
    }
}

pub(crate) fn version(context: &Context, _: HostVersionParams) -> HostVersionResult {
    HostVersionResult {
        wispd: VERSION.to_owned(),
        protocol: ProtocolRange::SUPPORTED,
        os: context.daemon.os.clone(),
        arch: std::env::consts::ARCH.to_owned(),
    }
}

/// The operating system and its version, such as `macOS 27.0`, read once at startup.
pub(crate) fn os_version() -> String {
    fs::read_to_string(SYSTEM_VERSION)
        .ok()
        .and_then(|plist| {
            let name = plist_string(&plist, "ProductName")?;
            let version = plist_string(&plist, "ProductVersion")?;
            Some(format!("{name} {version}"))
        })
        .unwrap_or_else(|| std::env::consts::OS.to_owned())
}

// The <string> after <key>key</key> in an XML property list.
fn plist_string<'a>(plist: &'a str, key: &str) -> Option<&'a str> {
    let after_key = &plist[plist.find(&format!("<key>{key}</key>"))?..];
    let start = after_key.find("<string>")? + "<string>".len();
    let end = start + after_key[start..].find("</string>")?;
    Some(after_key[start..end].trim())
}

#[cfg(test)]
mod tests {
    use super::{os_version, plist_string};

    #[test]
    fn reads_strings_from_a_property_list() {
        let plist = "<dict>\n\t<key>ProductBuildVersion</key>\n\t<string>27A1</string>\n\
                     \t<key>ProductName</key>\n\t<string>macOS</string>\n\
                     \t<key>ProductVersion</key>\n\t<string>27.0</string>\n</dict>";
        assert_eq!(plist_string(plist, "ProductName"), Some("macOS"));
        assert_eq!(plist_string(plist, "ProductVersion"), Some("27.0"));
        assert_eq!(plist_string(plist, "Missing"), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_os_version_names_macos() {
        assert!(os_version().starts_with("macOS "), "{}", os_version());
    }
}
