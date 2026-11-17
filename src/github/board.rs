//! arakun-pilot's board (`GET /board`): every pull request it follows, where
//! each one stands and who has it, in Spanish, plus one line that says
//! whether anything needs the user. The GitHub tab's Bandeja is drawn from it.
//!
//! Every field may be missing or null, and fields a newer Worker adds are
//! ignored: the board must never fail to read because pilot grew.

use serde::{Deserialize, Deserializer};

use super::pilot::{Config, OPCIONES_MAX, Opcion, or_default, pull_ref};
use super::pr::ItemRef;

/// A board file to read instead of the Worker (`ARAKUN_PILOT_BOARD`), for a
/// test Arakun and screenshots before the Worker has `/board`.
pub const FIXTURE_ENV: &str = "ARAKUN_PILOT_BOARD";

/// Where a pull request stands. `Listo` means approved and green: the
/// author merges, pilot never does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Estado {
    /// Only the user can move it.
    TeNecesita,
    /// pilot is on it right now.
    PilotTrabajando,
    /// A person other than the user has it (a review, an answer).
    Esperando,
    /// CI is still running.
    EsperandoCi,
    /// Approved and green: one merge away.
    Listo,
    /// Something broke that pilot can't fix alone.
    Atorado,
    /// A state from a newer Worker.
    #[default]
    #[serde(other)]
    Otro,
}

impl Estado {
    /// The order pilot sends them in: what needs the user, what is stuck,
    /// what moves on its own, what waits, what is one merge away.
    pub const ALL: [Estado; 7] = [
        Estado::TeNecesita,
        Estado::Atorado,
        Estado::PilotTrabajando,
        Estado::EsperandoCi,
        Estado::Esperando,
        Estado::Listo,
        Estado::Otro,
    ];

    /// The group's heading in the Bandeja.
    pub fn group_label(self) -> &'static str {
        match self {
            Estado::TeNecesita => "Needs you",
            Estado::PilotTrabajando => "pilot is on it",
            Estado::Esperando => "Waiting on others",
            Estado::EsperandoCi => "Waiting on CI",
            Estado::Listo => "Ready to merge",
            Estado::Atorado => "Stuck",
            Estado::Otro => "Other",
        }
    }

    /// The chip and the breadcrumb.
    pub fn label(self) -> &'static str {
        match self {
            Estado::Esperando => "Waiting",
            Estado::Listo => "Ready",
            other => other.group_label(),
        }
    }
}

/// Who has the pull request now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tipo {
    Tu,
    Pilot,
    Persona,
    Ci,
    #[default]
    #[serde(other)]
    Otro,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct LoTiene {
    #[serde(default, deserialize_with = "or_default")]
    pub login: String,
    #[serde(default, deserialize_with = "or_default")]
    pub nombre: String,
    #[serde(default, deserialize_with = "or_default")]
    pub tipo: Tipo,
}

impl LoTiene {
    /// The name to show: "You" for the user, else the name or the login.
    pub fn shown(&self) -> String {
        match self.tipo {
            Tipo::Tu => "You".into(),
            Tipo::Pilot if self.nombre.is_empty() => "pilot".into(),
            Tipo::Ci if self.nombre.is_empty() => "CI".into(),
            _ if !self.nombre.trim().is_empty() => self.nombre.trim().to_string(),
            _ => self.login.clone(),
        }
    }
}

/// pilot's last move on the pull request.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct UltimoPilot {
    #[serde(default, deserialize_with = "or_default")]
    pub texto: String,
    #[serde(default, deserialize_with = "or_default")]
    pub at: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Checks {
    #[serde(default, deserialize_with = "or_default")]
    pub pasaron: u32,
    #[serde(default, deserialize_with = "or_default")]
    pub total: u32,
    #[serde(default, deserialize_with = "or_default")]
    pub corriendo: u32,
    /// The failing checks' names. A name or `{"name": ...}` each.
    #[serde(default, deserialize_with = "names")]
    pub fallando: Vec<String>,
}

/// A list of names, each a string or an object with `name`, `nombre` or `id`.
fn names<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let list = Option::<Vec<serde_json::Value>>::deserialize(d)?.unwrap_or_default();
    Ok(list
        .iter()
        .filter_map(|v| match v {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Object(o) => ["name", "nombre", "id"]
                .iter()
                .find_map(|k| o.get(*k).and_then(|n| n.as_str()).map(str::to_string)),
            _ => None,
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect())
}

/// A question pilot stopped on, answered with `POST /items/<item>/answer`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Decision {
    /// The id of the item to answer.
    #[serde(default, deserialize_with = "or_default")]
    pub item: String,
    #[serde(default, deserialize_with = "or_default")]
    pub pregunta: String,
    #[serde(default, deserialize_with = "or_default")]
    pub opciones: Vec<Opcion>,
}

/// What kind of step a timeline line is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PasoTipo {
    Pilot,
    Revision,
    Commit,
    Ci,
    Merge,
    Pregunta,
    #[default]
    #[serde(other)]
    Otro,
}

/// One line of "Pilot activity".
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Paso {
    #[serde(default, deserialize_with = "or_default")]
    pub at: String,
    #[serde(default, deserialize_with = "or_default")]
    pub texto: String,
    #[serde(default, deserialize_with = "or_default")]
    pub tipo: PasoTipo,
}

/// A pull request on the board.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct BoardPr {
    #[serde(default, deserialize_with = "or_default")]
    pub repo: String,
    #[serde(default, deserialize_with = "or_default")]
    pub number: u64,
    #[serde(default, deserialize_with = "or_default")]
    pub title: String,
    #[serde(default, deserialize_with = "or_default")]
    pub author: String,
    #[serde(default, deserialize_with = "or_default")]
    pub url: String,
    #[serde(default, deserialize_with = "or_default")]
    pub head: String,
    #[serde(default, deserialize_with = "or_default")]
    pub branch: String,
    #[serde(default, deserialize_with = "or_default")]
    pub base: String,
    #[serde(default, deserialize_with = "or_default")]
    pub draft: bool,
    /// The pull request this one is stacked on.
    #[serde(default, deserialize_with = "or_default")]
    pub va_encima_de: Option<u64>,
    #[serde(default, deserialize_with = "or_default")]
    pub estado: Estado,
    #[serde(default, deserialize_with = "or_default")]
    pub lo_tiene: LoTiene,
    #[serde(default, deserialize_with = "or_default")]
    pub siguiente: String,
    /// Since when it is in this state (ISO).
    #[serde(default, deserialize_with = "or_default")]
    pub desde: String,
    #[serde(default, deserialize_with = "or_default")]
    pub ultimo_pilot: Option<UltimoPilot>,
    #[serde(default, deserialize_with = "or_default")]
    pub que_pasa: Option<String>,
    #[serde(default, deserialize_with = "or_default")]
    pub checks: Checks,
    #[serde(default, deserialize_with = "or_default")]
    pub al_fusionar: Option<String>,
    #[serde(default, deserialize_with = "or_default")]
    pub decision: Option<Decision>,
    #[serde(default, deserialize_with = "or_default")]
    pub timeline: Vec<Paso>,
}

impl BoardPr {
    /// The timeline oldest first, whatever order it came in.
    pub fn steps(&self) -> Vec<&Paso> {
        let mut v: Vec<&Paso> = self.timeline.iter().collect();
        v.sort_by_key(|p| crate::ci::parse_time(&p.at).unwrap_or(0));
        v
    }

    pub fn item_ref(&self) -> Option<ItemRef> {
        pull_ref(&self.repo, self.number, &self.url)
    }

    /// The row's one line: what comes next, else pilot's last move.
    pub fn line(&self) -> String {
        let next = self.siguiente.trim();
        if !next.is_empty() {
            return next.to_string();
        }
        self.ultimo_pilot
            .as_ref()
            .map(|u| u.texto.trim().to_string())
            .unwrap_or_default()
    }

    /// The decision, when it can be answered: an item and a question. Its
    /// options may be missing; the user can always answer in their own words.
    pub fn open_decision(&self) -> Option<&Decision> {
        self.decision
            .as_ref()
            .filter(|d| !d.item.trim().is_empty() && !d.pregunta.trim().is_empty())
    }
}

/// A pull request merged today.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Hecho {
    #[serde(default, deserialize_with = "or_default")]
    pub repo: String,
    #[serde(default, deserialize_with = "or_default")]
    pub number: u64,
    #[serde(default, deserialize_with = "or_default")]
    pub title: String,
    #[serde(default, deserialize_with = "or_default")]
    pub merged_at: String,
}

impl Hecho {
    pub fn item_ref(&self) -> Option<ItemRef> {
        // No address: a merge without a number names nothing.
        pull_ref(&self.repo, self.number, "")
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Verdict {
    #[serde(default, deserialize_with = "or_default")]
    pub te_necesita: u32,
    #[serde(default, deserialize_with = "or_default")]
    pub pilot_lleva: u32,
    #[serde(default, deserialize_with = "or_default")]
    pub texto: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Board {
    #[serde(default, deserialize_with = "or_default")]
    pub at: String,
    #[serde(default, deserialize_with = "or_default")]
    pub verdict: Verdict,
    #[serde(default, deserialize_with = "or_default")]
    pub prs: Vec<BoardPr>,
    #[serde(default, deserialize_with = "or_default")]
    pub hecho_hoy: Vec<Hecho>,
}

impl Board {
    /// The pull request `number` of `repo` (any case).
    pub fn find(&self, repo: &str, number: u64) -> Option<&BoardPr> {
        self.prs
            .iter()
            .find(|p| p.number == number && p.repo.eq_ignore_ascii_case(repo))
    }

    /// The item ids of the questions it asks now.
    pub fn asking(&self) -> std::collections::HashSet<String> {
        self.prs
            .iter()
            .filter_map(|p| p.open_decision().map(|d| d.item.clone()))
            .collect()
    }

    /// The pull request whose decision is the item `id`.
    pub fn find_decision(&self, id: &str) -> Option<&BoardPr> {
        self.prs.iter().find(|p| {
            p.decision
                .as_ref()
                .is_some_and(|d| !id.is_empty() && d.item == id)
        })
    }

    /// The rows of one group in the order they show, each with whether it is
    /// drawn under its base: a stacked pull request follows the one it sits
    /// on when both are in the group, the rest keep the board's order.
    pub fn group(&self, estado: Estado) -> Vec<(&BoardPr, bool)> {
        let here: Vec<&BoardPr> = self.prs.iter().filter(|p| p.estado == estado).collect();
        let has_base = |p: &BoardPr| {
            p.va_encima_de.is_some_and(|b| {
                b != p.number
                    && here
                        .iter()
                        .any(|o| o.number == b && o.repo.eq_ignore_ascii_case(&p.repo))
            })
        };
        let mut out: Vec<(&BoardPr, bool)> = Vec::new();
        fn place<'a>(
            p: &'a BoardPr,
            here: &[&'a BoardPr],
            out: &mut Vec<(&'a BoardPr, bool)>,
            nested: bool,
            depth: usize,
        ) {
            if out.iter().any(|(o, _)| std::ptr::eq(*o, p)) {
                return;
            }
            out.push((p, nested));
            // A loop in the stack (a bad board) stops here.
            if depth > 8 {
                return;
            }
            for child in here.iter().filter(|c| {
                c.va_encima_de == Some(p.number)
                    && c.number != p.number
                    && c.repo.eq_ignore_ascii_case(&p.repo)
            }) {
                place(child, here, out, true, depth + 1);
            }
        }
        for p in &here {
            if !has_base(p) {
                place(p, &here, &mut out, false, 0);
            }
        }
        // Whatever a loop kept out still shows.
        for p in &here {
            if !out.iter().any(|(o, _)| std::ptr::eq(*o, *p)) {
                out.push((p, false));
            }
        }
        out
    }
}

/// The queue's rows pilot doesn't follow, done ones aside: review requests
/// elsewhere, mentions, issues. The Bandeja folds them under "More on GitHub".
pub fn outside<'a>(
    b: &'a Board,
    q: &'a super::queue::Queue,
) -> impl Iterator<Item = &'a (super::queue::Entry, super::queue::Placed)> {
    q.rows
        .iter()
        .filter(|(_, p)| p.group != super::queue::Group::Done)
        .filter(|(e, _)| {
            e.item
                .as_ref()
                .is_none_or(|i| b.find(&i.repo, i.number).is_none())
        })
}

/// How many things need the user with a board: what pilot says, plus the
/// queue's rows on the user that pilot doesn't follow.
pub fn needs_you(b: &Board, q: &super::queue::Queue) -> usize {
    let elsewhere = outside(b, q)
        .filter(|(_, p)| p.lane == super::queue::Lane::OnYou)
        .count();
    b.verdict.te_necesita as usize + elsewhere
}

/// Where one of pilot's cards shows when there is a board.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardPlace {
    /// It is the board's question on a pull request: the sheet asks it.
    Question,
    /// Its pull request is on the board: "From pilot" in that sheet.
    Sheet,
    /// The board doesn't follow its pull request, or it names none: the
    /// Bandeja's own "From pilot" group.
    Loose,
}

pub fn card_place(b: &Board, card: &super::pilot::Item) -> CardPlace {
    // A question counts as the board's only where its row can open: an
    // entry that names no pull request has no row, so the card shows.
    if b.find_decision(&card.id)
        .is_some_and(|p| p.item_ref().is_some())
    {
        return CardPlace::Question;
    }
    match card.item_ref() {
        Some(i) if b.find(&i.repo, i.number).is_some() => CardPlace::Sheet,
        _ => CardPlace::Loose,
    }
}

/// Everything that waits on the user with a board, for the headline and the
/// badge alike: what pilot says, the queue's rows on the user it doesn't
/// follow, and pilot's cards that aren't one of its questions.
pub fn waiting<'a>(
    b: &Board,
    q: &super::queue::Queue,
    cards: impl Iterator<Item = &'a super::pilot::Item>,
) -> usize {
    needs_you(b, q) + extra_cards(b, cards)
}

/// The cards that wait on top of what the board counts: every one but its
/// questions, which `te_necesita` already has.
pub fn extra_cards<'a>(b: &Board, cards: impl Iterator<Item = &'a super::pilot::Item>) -> usize {
    cards
        .filter(|c| card_place(b, c) != CardPlace::Question)
        .count()
}

/// What `GET /board` answered, cleaned: options without an id or words go,
/// as on the cards, and pull requests without a number.
pub fn parse_board(bytes: &[u8]) -> Result<Board, String> {
    let mut b: Board =
        serde_json::from_slice(bytes).map_err(|e| format!("unexpected answer: {e}"))?;
    b.prs.retain(|p| p.number > 0);
    for p in &mut b.prs {
        // One identity for every lookup: an entry named only by its address
        // gets the repository (and number) that address names.
        if let Some(r) = p.item_ref() {
            p.repo = r.repo;
            p.number = r.number;
        }
        if let Some(d) = &mut p.decision {
            d.opciones
                .retain(|o| !o.id.trim().is_empty() && !o.texto.trim().is_empty());
            d.opciones.truncate(OPCIONES_MAX);
        }
    }
    Ok(b)
}

/// Why the board couldn't be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BoardProblem {
    /// The Worker has no `/board` yet (404): the tab shows the plain queue.
    Missing,
    Failed(String),
}

/// Reads the board: the fixture file when `ARAKUN_PILOT_BOARD` names one,
/// else the Worker with the read token.
pub fn fetch(cfg: Option<&Config>) -> Result<Board, BoardProblem> {
    if let Some(path) = std::env::var_os(FIXTURE_ENV) {
        let bytes = std::fs::read(&path).map_err(|e| {
            BoardProblem::Failed(format!(
                "could not read {}: {e}",
                std::path::Path::new(&path).display()
            ))
        })?;
        return parse_board(&bytes).map_err(BoardProblem::Failed);
    }
    let Some(cfg) = cfg else {
        return Err(BoardProblem::Missing);
    };
    let (status, bytes) =
        super::pilot::get(cfg, "/board", 8 * 1024 * 1024).map_err(BoardProblem::Failed)?;
    match status {
        200 => parse_board(&bytes).map_err(BoardProblem::Failed),
        404 => Err(BoardProblem::Missing),
        s => Err(BoardProblem::Failed(super::pilot::status_problem(s))),
    }
}

/// "now", "12 min ago", "1 h 20 min ago", "3 d ago"; empty for an unknown time.
pub fn hace(at: &str) -> String {
    let Some(t) = crate::ci::parse_time(at) else {
        return String::new();
    };
    let s = crate::model::unix_now().saturating_sub(t);
    if s < 60 {
        span(s)
    } else {
        format!("{} ago", span(s))
    }
}

/// A short span: "now", "12 min", "1 h 20 min", "3 d".
pub fn span(secs: u64) -> String {
    let (m, h, d) = (secs / 60, secs / 3600, secs / 86_400);
    match secs {
        0..60 => "now".into(),
        60..3600 => format!("{m} min"),
        3600..86_400 if m % 60 == 0 => format!("{h} h"),
        3600..86_400 => format!("{h} h {} min", m % 60),
        _ => format!("{d} d"),
    }
}

/// The span since `at` alone ("12 min"), for the rows' right edge.
pub fn since(at: &str) -> String {
    crate::ci::parse_time(at)
        .map(|t| span(crate::model::unix_now().saturating_sub(t)))
        .unwrap_or_default()
}

/// The mockups' board: every estado, a stack, a decision.
#[cfg(test)]
pub(crate) const FIXTURE: &str = include_str!("board_fixture.json");
/// A real answer of `GET /board` (2026-11-17): two of Dani's pull requests,
/// one stacked on the other, and a long day of merges.
#[cfg(test)]
const REAL: &str = include_str!("board_real.json");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_fixture() {
        let b = parse_board(FIXTURE.as_bytes()).unwrap();
        assert_eq!(b.verdict.te_necesita, 1);
        assert!(b.prs.len() >= 5);
        let p = b.find("arakunhq/arakun", 151).unwrap();
        assert_eq!(p.estado, Estado::TeNecesita);
        assert_eq!(p.lo_tiene.tipo, Tipo::Tu);
        let d = p.open_decision().unwrap();
        assert!(d.opciones.iter().all(|o| !o.texto.is_empty()));
        assert_eq!(b.find_decision(&d.item).map(|p| p.number), Some(151));
        // Its question is what a failed answer's note is kept for.
        assert!(b.asking().contains(&d.item));
        assert!(!b.hecho_hoy.is_empty());
    }

    #[test]
    fn reads_a_real_board() {
        let b = parse_board(REAL.as_bytes()).unwrap();
        assert_eq!(b.verdict.te_necesita, 0);
        assert!(b.verdict.texto.starts_with("Nada te necesita"));
        assert_eq!(b.prs.len(), 2);
        assert_eq!(b.hecho_hoy.len(), 52);
        // #185 sits on #184: it follows it, drawn under it.
        let order: Vec<(u64, bool)> = b
            .group(Estado::Esperando)
            .iter()
            .map(|(p, n)| (p.number, *n))
            .collect();
        assert_eq!(order, [(184, false), (185, true)]);
        // The Worker sends the newest step first; the sheet shows the oldest first.
        let p = b.find("ArakunHQ/arakun", 184).unwrap();
        let steps = p.steps();
        assert!(
            steps
                .windows(2)
                .all(|w| { crate::ci::parse_time(&w[0].at) <= crate::ci::parse_time(&w[1].at) })
        );
        assert_eq!(steps.last().unwrap().texto, "Pasaron los 23 checks");
    }

    #[test]
    fn missing_and_null_fields_read_as_empty() {
        let b = parse_board(
            br#"{"prs": [
                {"number": 7, "estado": "algo_nuevo", "lo_tiene": null, "checks": {"fallando": [{"name": "lint"}, "test", 3]},
                 "decision": {"item": "x", "pregunta": "?", "opciones": [{"id": "", "texto": "a"}, {"id": "b", "texto": "B"}]},
                 "extra": {"nuevo": true}},
                {"number": 8, "lo_tiene": {"login": null, "nombre": "alguien", "tipo": "persona"}},
                {"title": "sin numero"}
            ], "verdict": null}"#,
        )
        .unwrap();
        assert_eq!(b.prs.len(), 2);
        assert_eq!(b.prs[1].lo_tiene.shown(), "alguien");
        assert_eq!(b.prs[1].lo_tiene.login, "");
        let p = &b.prs[0];
        assert_eq!(p.estado, Estado::Otro);
        assert_eq!(p.lo_tiene, LoTiene::default());
        assert_eq!(p.checks.fallando, ["lint", "test"]);
        assert_eq!(p.decision.as_ref().unwrap().opciones.len(), 1);
        assert_eq!(b.verdict, Verdict::default());
        assert!(parse_board(b"<html>").is_err());
    }

    #[test]
    fn a_stacked_pull_follows_its_base() {
        let pr = |n: u64, on: Option<u64>| BoardPr {
            repo: "o/r".into(),
            number: n,
            va_encima_de: on,
            estado: Estado::Esperando,
            ..BoardPr::default()
        };
        let b = Board {
            prs: vec![
                pr(121, Some(120)),
                pr(5, None),
                pr(120, None),
                pr(9, Some(77)),
            ],
            ..Board::default()
        };
        let order: Vec<(u64, bool)> = b
            .group(Estado::Esperando)
            .iter()
            .map(|(p, n)| (p.number, *n))
            .collect();
        // #9's base is elsewhere: it shows on its own.
        assert_eq!(order, [(5, false), (120, false), (121, true), (9, false)]);

        // A loop still shows every pull request once.
        let looped = Board {
            prs: vec![pr(1, Some(2)), pr(2, Some(1))],
            ..Board::default()
        };
        assert_eq!(looped.group(Estado::Esperando).len(), 2);
    }

    #[test]
    fn what_pilot_does_not_follow_still_needs_you() {
        use crate::github::queue::{Entry, Group, Lane, Placed, Queue};
        let row = |repo: &str, n: u64, group: Group, lane: Lane| {
            let entry = Entry {
                item: ItemRef::parse(&format!("{repo}#{n}")),
                title: String::new(),
                author: String::new(),
                updated_at: String::new(),
                closed_at: String::new(),
                state: String::new(),
                why: Vec::new(),
                notification: None,
            };
            let placed = Placed {
                group,
                lane,
                ..Placed::default()
            };
            (entry, placed)
        };
        let b = Board {
            verdict: Verdict {
                te_necesita: 1,
                ..Verdict::default()
            },
            prs: vec![BoardPr {
                repo: "o/r".into(),
                number: 7,
                ..BoardPr::default()
            }],
            ..Board::default()
        };
        let q = Queue {
            rows: vec![
                // pilot follows it: its count is pilot's.
                row("O/R", 7, Group::NeedsReview, Lane::OnYou),
                // A review asked elsewhere counts.
                row("x/y", 1, Group::NeedsReview, Lane::OnYou),
                // Someone else's move, and what is done, don't.
                row("x/y", 2, Group::Yours, Lane::OnOthers),
                row("x/y", 3, Group::Done, Lane::OnYou),
            ],
            ..Queue::default()
        };
        assert_eq!(outside(&b, &q).count(), 2);
        assert_eq!(needs_you(&b, &q), 2);
    }

    #[test]
    fn an_entry_named_by_its_address_is_found_by_its_repository() {
        let b = parse_board(
            br#"{"prs": [{"number": 7, "url": "https://github.com/ArakunHQ/arakun/pull/7"}]}"#,
        )
        .unwrap();
        assert_eq!(b.find("arakunhq/arakun", 7).map(|p| p.number), Some(7));
    }

    #[test]
    fn every_card_has_a_place_with_a_board() {
        use super::super::pilot::{Item, Kind};
        let card = |id: &str, kind: Kind, repo: &str, number: u64| Item {
            id: id.into(),
            kind,
            repo: repo.into(),
            number,
            ..Item::default()
        };
        let b = Board {
            verdict: Verdict {
                te_necesita: 1,
                ..Verdict::default()
            },
            prs: vec![BoardPr {
                repo: "ArakunHQ/arakun".into(),
                number: 7,
                decision: Some(Decision {
                    item: "q1".into(),
                    ..Decision::default()
                }),
                ..BoardPr::default()
            }],
            ..Board::default()
        };
        let cards = [
            // The board's own question: the sheet asks it.
            card("q1", Kind::NeedsYou, "ArakunHQ/arakun", 7),
            // A brief on a pull request the board follows: in its sheet.
            card("b1", Kind::Brief, "arakunhq/arakun", 7),
            // A fix on a pull request the board doesn't follow, and a failure about none.
            card("f1", Kind::Fixed, "ArakunHQ/arakun", 3),
            card("e1", Kind::Failed, "", 0),
        ];
        let places: Vec<CardPlace> = cards.iter().map(|c| card_place(&b, c)).collect();
        assert_eq!(
            places,
            [
                CardPlace::Question,
                CardPlace::Sheet,
                CardPlace::Loose,
                CardPlace::Loose
            ]
        );
        // The question is already in te_necesita; the rest add to it.
        assert_eq!(extra_cards(&b, cards.iter()), 3);
        // A question on an entry that names no pull request has no row to
        // open: its card stays in view.
        let nowhere = Board {
            prs: vec![BoardPr {
                number: 8,
                decision: Some(Decision {
                    item: "q2".into(),
                    ..Decision::default()
                }),
                ..BoardPr::default()
            }],
            ..Board::default()
        };
        let q2 = card("q2", Kind::NeedsYou, "", 0);
        assert_eq!(card_place(&nowhere, &q2), CardPlace::Loose);
        // The headline and the badge both count te_necesita and those cards.
        let q = crate::github::queue::Queue::default();
        assert_eq!(waiting(&b, &q, cards.iter()), 4);
    }

    #[test]
    fn the_row_says_what_comes_next() {
        let mut p = BoardPr {
            ultimo_pilot: Some(UltimoPilot {
                texto: "Subió 1 commit".into(),
                at: String::new(),
            }),
            ..BoardPr::default()
        };
        assert_eq!(p.line(), "Subió 1 commit");
        p.siguiente = "Rik revisa la ronda 2".into();
        assert_eq!(p.line(), "Rik revisa la ronda 2");
        assert_eq!(span(30), "now");
        assert_eq!(span(4800), "1 h 20 min");
        assert_eq!(span(3 * 86_400 + 5), "3 d");
    }
}
