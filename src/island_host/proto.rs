//! What goes over the island's socket: one JSON object per line. The app
//! opens with `Hello` and keeps the connection: feeds and alerts go one way,
//! the presses it handles come back. Any other first line is a one-shot
//! request (`arakun island …`), answered with one [`Reply`] line.

use std::path::PathBuf;

use gpui::SharedString;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub use crate::control::Reply;
use crate::island::{Alert, AlertKind, Feed, Phase};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToIsland {
    /// The app, staying connected; it goes on in the background when the app quits.
    Hello {
        pid: u32,
    },
    /// What every session is up to, sent when it changes.
    Feed {
        feed: Feed,
    },
    Alert {
        alert: Alert,
    },
    /// Shows a phase whatever the agents do (`None`: follow them again); unpins the list.
    Hold {
        phase: Option<Phase>,
    },
    /// Pins the list; `None` flips it.
    Pin {
        on: Option<bool>,
    },
    /// Presses a button of the card showing.
    Press {
        button: Button,
    },
    /// The phase it shows.
    State,
    /// Frames painted in the last two seconds.
    Fps,
    /// Ends the island's process: the app sends it when the island is turned
    /// off in Settings, since the host outlives the app and has no window to close.
    Quit,
    /// Starts or ends a voice call; `None` flips it.
    #[cfg(feature = "coderlabs")]
    Voice {
        on: Option<bool>,
    },
    /// What the wake listener says ("maybe", "yes", "no"), for testing that path by hand.
    #[cfg(feature = "coderlabs")]
    Wake {
        state: String,
    },
    /// Starts or ends recording a meeting; `None` flips it. `people`: how many
    /// voices the others are, when known, to split them better.
    #[cfg(feature = "coderlabs")]
    Meeting {
        on: Option<bool>,
        #[serde(default)]
        people: Option<u32>,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Button {
    Ver,
    Luego,
    Aprobar,
    Rechazar,
}

/// What the island asks of the app.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromIsland {
    /// The card's "Ver".
    Ver {
        key: SharedString,
    },
    Luego {
        key: SharedString,
    },
    /// A row of the list.
    Row {
        key: SharedString,
    },
}

/// `$XDG_RUNTIME_DIR/arakun/island.sock`; a test instance's own with
/// `ARAKUN_ISLAND_SOCKET`, or next to its separate state file.
pub fn socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("ARAKUN_ISLAND_SOCKET") {
        return p.into();
    }
    if let Some(state) = std::env::var_os("ARAKUN_STATE") {
        return PathBuf::from(state).with_extension("island.sock");
    }
    crate::control::runtime_dir().join("island.sock")
}

/// One line, newline included.
pub fn encode<T: Serialize>(msg: &T) -> String {
    let mut line = serde_json::to_string(msg).unwrap_or_default();
    line.push('\n');
    line
}

pub fn decode<T: DeserializeOwned>(line: &str) -> Result<T, String> {
    serde_json::from_str(line.trim_end()).map_err(|e| format!("bad message: {e}"))
}

/// `arakun island`'s words (as `control::island_args` checked them) as a request.
pub fn from_args(args: &[String]) -> Option<ToIsland> {
    let button = |button| Some(ToIsland::Press { button });
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        [] => Some(ToIsland::State),
        ["alert", title, subtitle @ ..] => Some(ToIsland::Alert {
            alert: Alert {
                key: SharedString::default(),
                kind: AlertKind::Finished,
                title: title.to_string().into(),
                subtitle: subtitle
                    .first()
                    .copied()
                    .unwrap_or_default()
                    .to_string()
                    .into(),
                req: SharedString::default(),
                image: SharedString::default(),
                brand: SharedString::default(),
            },
        }),
        ["ver"] => button(Button::Ver),
        ["luego"] => button(Button::Luego),
        ["aprobar"] => button(Button::Aprobar),
        ["rechazar"] => button(Button::Rechazar),
        ["list"] => Some(ToIsland::Pin { on: None }),
        ["fps"] => Some(ToIsland::Fps),
        ["auto"] => Some(ToIsland::Hold { phase: None }),
        #[cfg(feature = "coderlabs")]
        ["wake", s @ ("maybe" | "yes" | "no")] => Some(ToIsland::Wake {
            state: s.to_string(),
        }),
        [name] => Phase::from_name(name).map(|phase| ToIsland::Hold { phase: Some(phase) }),
        _ => None,
    }
}

/// `arakun reunion [on|off] [personas]`.
#[cfg(feature = "coderlabs")]
pub fn reunion_args(args: &[String]) -> Option<ToIsland> {
    let people = args.iter().find_map(|a| a.parse::<u32>().ok());
    let rest: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|a| a.parse::<u32>().is_err())
        .collect();
    let on = match rest.as_slice() {
        [] | ["toggle"] => None,
        ["on"] => Some(true),
        ["off"] => Some(false),
        _ => return None,
    };
    Some(ToIsland::Meeting { on, people })
}

/// `arakun voz [on|off]`.
#[cfg(feature = "coderlabs")]
pub fn voz_args(args: &[String]) -> Option<ToIsland> {
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] | ["toggle"] => Some(ToIsland::Voice { on: None }),
        ["on"] => Some(ToIsland::Voice { on: Some(true) }),
        ["off"] => Some(ToIsland::Voice { on: Some(false) }),
        _ => None,
    }
}

/// Sends one request and waits for its answer.
#[cfg(unix)]
pub fn request(msg: &ToIsland) -> std::io::Result<Reply> {
    use std::io::{BufRead as _, BufReader, Write as _};
    let mut stream = std::os::unix::net::UnixStream::connect(socket_path())?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
    stream.write_all(encode(msg).as_bytes())?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    decode(&line).map_err(std::io::Error::other)
}

/// Windows 10+ has AF_UNIX sockets too; `uds_windows` exposes them.
#[cfg(windows)]
pub fn request(msg: &ToIsland) -> std::io::Result<Reply> {
    use std::io::{BufRead as _, BufReader, Write as _};
    let mut stream = uds_windows::UnixStream::connect(socket_path())?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
    stream.write_all(encode(msg).as_bytes())?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    decode(&line).map_err(std::io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::island::{Row, Status};

    fn s(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn messages_round_trip_as_lines() {
        let feed = Feed {
            rows: vec![Row {
                key: "s1".into(),
                status: Status::Waiting,
                model: "Opus 5.5".into(),
                doing: "¿Aplico el parche?".into(),
                elapsed: "3:07".into(),
                cost: "$1.92".into(),
                updated: 7,
            }],
            today: Some("$2.87".into()),
            viewing: None,
        };
        let all = [
            ToIsland::Hello { pid: 42 },
            ToIsland::Feed { feed },
            ToIsland::Hold {
                phase: Some(Phase::Listening),
            },
            ToIsland::Pin { on: None },
            ToIsland::Press {
                button: Button::Aprobar,
            },
            ToIsland::State,
            ToIsland::Fps,
            ToIsland::Quit,
        ];
        for msg in all {
            let line = encode(&msg);
            assert!(line.ends_with('\n') && !line[..line.len() - 1].contains('\n'));
            assert_eq!(decode::<ToIsland>(&line).unwrap(), msg);
        }
        assert_eq!(
            encode(&ToIsland::Hello { pid: 1 }),
            "{\"type\":\"hello\",\"pid\":1}\n"
        );
        let line = encode(&ToIsland::Feed {
            feed: Feed::default(),
        });
        assert!(line.contains("\"type\":\"feed\""));
        let back = encode(&FromIsland::Row { key: "s1".into() });
        assert_eq!(back, "{\"type\":\"row\",\"key\":\"s1\"}\n");
        assert_eq!(
            decode::<FromIsland>(&back).unwrap(),
            FromIsland::Row { key: "s1".into() }
        );
        // A notice that isn't a session's, and "Ver" coming back with what it opens.
        let notice = ToIsland::Alert {
            alert: Alert {
                key: "open:run:42:/home/k/app".into(),
                kind: AlertKind::Failure,
                title: "Deploy failed · main".into(),
                subtitle: "app · deploy — fix".into(),
                req: SharedString::default(),
                image: SharedString::default(),
                brand: SharedString::default(),
            },
        };
        assert_eq!(decode::<ToIsland>(&encode(&notice)).unwrap(), notice);
        assert!(encode(&notice).contains("\"kind\":\"failure\""));
        let ver = FromIsland::Ver {
            key: "open:run:42:/home/k/app".into(),
        };
        assert_eq!(decode::<FromIsland>(&encode(&ver)).unwrap(), ver);
        // An alert from before confirmations has no request.
        let a: ToIsland = decode(
            r#"{"type":"alert","alert":{"key":"","kind":"input","title":"t","subtitle":""}}"#,
        )
        .unwrap();
        assert!(
            matches!(a, ToIsland::Alert { alert } if alert.kind == AlertKind::Input && alert.req.is_empty())
        );
        assert!(decode::<ToIsland>("{\"type\":\"dance\"}").is_err());
    }

    #[test]
    fn cli_words_become_requests() {
        assert_eq!(from_args(&s(&[])), Some(ToIsland::State));
        assert_eq!(
            from_args(&s(&["working"])),
            Some(ToIsland::Hold {
                phase: Some(Phase::Working)
            })
        );
        assert_eq!(
            from_args(&s(&["auto"])),
            Some(ToIsland::Hold { phase: None })
        );
        assert_eq!(from_args(&s(&["list"])), Some(ToIsland::Pin { on: None }));
        assert_eq!(
            from_args(&s(&["rechazar"])),
            Some(ToIsland::Press {
                button: Button::Rechazar
            })
        );
        assert_eq!(from_args(&s(&["fps"])), Some(ToIsland::Fps));
        match from_args(&s(&["alert", "GLM terminó", "listo"])) {
            Some(ToIsland::Alert { alert }) => assert_eq!(
                (alert.title.as_ref(), alert.subtitle.as_ref()),
                ("GLM terminó", "listo")
            ),
            other => panic!("{other:?}"),
        }
        assert_eq!(from_args(&s(&["dancing"])), None);
    }

    #[test]
    #[cfg(feature = "coderlabs")]
    fn voz_toggles_or_sets_the_call() {
        assert_eq!(voz_args(&s(&[])), Some(ToIsland::Voice { on: None }));
        assert_eq!(
            voz_args(&s(&["toggle"])),
            Some(ToIsland::Voice { on: None })
        );
        assert_eq!(
            voz_args(&s(&["on"])),
            Some(ToIsland::Voice { on: Some(true) })
        );
        assert_eq!(
            voz_args(&s(&["off"])),
            Some(ToIsland::Voice { on: Some(false) })
        );
        assert_eq!(voz_args(&s(&["on", "off"])), None);
        assert_eq!(voz_args(&s(&["maybe"])), None);
        let line = encode(&ToIsland::Voice { on: Some(true) });
        assert_eq!(line, "{\"type\":\"voice\",\"on\":true}\n");
        assert_eq!(
            decode::<ToIsland>(&line).unwrap(),
            ToIsland::Voice { on: Some(true) }
        );
    }
}
