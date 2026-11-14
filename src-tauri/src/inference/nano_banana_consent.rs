//! The consent a Nano Banana render spends.
//!
//! Settings' Cloud engines switch is a value the webview writes, so on its own
//! it says nothing about whether the person agreed to this request. The window
//! in front of every Nano Banana request (`nanobananaask.svelte.js`) answers
//! that: when the person confirms it, the interface asks for a grant here, and
//! every Nano Banana render presents one (`region.rs#edit`). The cloud GPU's
//! grants bind to one stored region's bytes; this one is smaller, because a
//! Nano Banana request is one blocking call on the person's own key, with no
//! worker left running:
//!
//! - **What was confirmed.** The model and the instruction are read from
//!   settings when the grant is made, and a render uses the grant's copy. An
//!   instruction edited during a batch is for the next window, not the rest of
//!   this one.
//! - **How many regions.** Each render takes one, whether or not it then needs
//!   a request: an answer shared from an earlier crop still counts, so the
//!   count is never more than the window said.
//! - **Which chapter.** A grant names the chapter its window was opened on,
//!   and a render or a batch on another chapter is refused it.
//! - **For how long.** A grant lapses [`IDLE`] after it was made or after its
//!   last render finished. A render can take many minutes (several crops, a
//!   retry each, a long timeout), so the clock is not running while one holds
//!   a share, retries between its waits included.
//! - **Whose.** Each window's grant is its own, so a second window does not
//!   end the first one's renders: a batch goes on while a single region is
//!   confirmed elsewhere. A batch claims its grant when it starts
//!   ([`Book::claim`]), and from then on only that batch spends it.
//! - **Stopped.** Each grant carries the [`Stop`] its renders watch, taken
//!   when a render spends its share. Withdrawing the grant raises it, so a
//!   Stop that lands at any point after the share was spent ends that render
//!   before it sends, or while it waits on Google. Stop on one region or one
//!   batch withdraws that grant only ([`Book::revoke`],
//!   [`Book::revoke_owned_by`]); turning Cloud engines off and quitting
//!   withdraw them all ([`revoke_all`]).

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;

use super::nano_banana::{Config, Granted, Stop};

/// How long a grant waits, unused, before it lapses.
pub const IDLE: Duration = Duration::from_secs(10 * 60);

/// The most regions one grant covers. A whole chapter's batch is far below it.
/// The window refuses a larger count before it opens
/// (`nanobananaask.svelte.js`).
pub const MAX_REGIONS: u32 = 2_000;

/// Why a render may not go.
pub(crate) const NO_CONSENT: &str = "decline.reason.nanoBananaNoConsent";
/// Cloud engines are off: nothing may be sent, grant or no grant.
pub(crate) const CLOUD_OFF: &str = "decline.reason.nanoBananaOff";

struct Live {
    nonce: String,
    config: Config,
    /// The chapter the window was opened on.
    chapter: String,
    left: u32,
    lapses: Instant,
    /// Renders holding a share right now. The grant does not lapse while one does.
    spending: u32,
    /// The batch that claimed the grant ([`Book::claim`]).
    owner: Option<String>,
    stop: Stop,
}

impl Live {
    fn lapsed(&self, now: Instant) -> bool {
        self.spending == 0 && now >= self.lapses
    }
}

/// The grants this process has made.
pub(crate) struct Book {
    live: Mutex<Vec<Live>>,
    idle: Duration,
}

/// One render's share of a grant: the settings the person confirmed and the
/// grant's stop. When it is dropped, the render is over and the grant's idle
/// clock starts again.
pub(crate) struct Spend<'a> {
    book: &'a Book,
    nonce: String,
    pub config: Config,
    pub stop: Stop,
}

impl Spend<'_> {
    /// What the render is handed.
    pub(crate) fn granted(&self) -> Granted {
        Granted { config: self.config.clone(), stop: self.stop.clone() }
    }
}

impl Drop for Spend<'_> {
    fn drop(&mut self) {
        let mut live = self.book.lock();
        if let Some(grant) = live.iter_mut().find(|grant| grant.nonce == self.nonce) {
            grant.spending = grant.spending.saturating_sub(1);
            grant.lapses = Instant::now() + self.book.idle;
        }
    }
}

impl Book {
    pub(crate) const fn new(idle: Duration) -> Book {
        Book { live: Mutex::new(Vec::new()), idle }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Live>> {
        self.live.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Take out the grants `gone` picks, raising each one's stop.
    fn withdraw(live: &mut Vec<Live>, gone: impl Fn(&Live) -> bool) {
        live.retain(|grant| {
            let going = gone(grant);
            if going {
                grant.stop.raise();
            }
            !going
        });
    }

    /// A grant for `count` renders on `chapter` with `config`. Other grants
    /// stand; the ones lapsed or used up are forgotten here.
    pub(crate) fn issue(&self, config: Config, chapter: &str, count: u32) -> Result<String, &'static str> {
        if count == 0 || count > MAX_REGIONS {
            return Err(NO_CONSENT);
        }
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|_| NO_CONSENT)?;
        let nonce: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let now = Instant::now();
        let mut live = self.lock();
        Self::withdraw(&mut live, |grant| grant.lapsed(now) || (grant.left == 0 && grant.spending == 0));
        live.push(Live {
            nonce: nonce.clone(),
            config,
            chapter: chapter.to_owned(),
            left: count,
            lapses: now + self.idle,
            spending: 0,
            owner: None,
            stop: Stop::default(),
        });
        Ok(nonce)
    }

    /// One render's share of the grant `nonce` names, for a render on
    /// `chapter` by `owner` (the batch's run id, or none for a single region),
    /// while Cloud engines are `cloud_allowed`. Nothing is taken when the
    /// answer is a refusal.
    pub(crate) fn spend(&self, cloud_allowed: bool, nonce: Option<&str>, chapter: &str, owner: Option<&str>)
        -> Result<Spend<'_>, &'static str> {
        if !cloud_allowed {
            return Err(CLOUD_OFF);
        }
        let nonce = nonce.ok_or(NO_CONSENT)?;
        let now = Instant::now();
        let mut live = self.lock();
        let Some(at) = live.iter().position(|grant| grant.nonce == nonce) else {
            return Err(NO_CONSENT);
        };
        if live[at].lapsed(now) {
            Self::withdraw(&mut live, |grant| grant.nonce == nonce);
            return Err(NO_CONSENT);
        }
        let grant = &mut live[at];
        if grant.left == 0 || grant.chapter != chapter || grant.owner.as_deref() != owner {
            return Err(NO_CONSENT);
        }
        grant.left -= 1;
        grant.spending += 1;
        Ok(Spend { book: self, nonce: nonce.to_owned(), config: grant.config.clone(), stop: grant.stop.clone() })
    }

    /// The name of the profile the grant `nonce` was made on, for the batch's
    /// report.
    pub(crate) fn profile_of(&self, nonce: &str) -> Option<String> {
        let live = self.lock();
        live.iter().find(|grant| grant.nonce == nonce)?.config.profile.as_ref().map(|profile| profile.name.clone())
    }

    /// How many renders the grant `nonce` names has left, without taking one:
    /// what a batch checks before it starts and between the retries of one
    /// region, which spend nothing more. Refused as [`Book::spend`] refuses.
    pub(crate) fn left(&self, cloud_allowed: bool, nonce: &str) -> Result<u32, &'static str> {
        if !cloud_allowed {
            return Err(CLOUD_OFF);
        }
        let now = Instant::now();
        let live = self.lock();
        match live.iter().find(|grant| grant.nonce == nonce) {
            Some(grant) if !grant.lapsed(now) => Ok(grant.left),
            _ => Err(NO_CONSENT),
        }
    }

    /// Bind the grant `nonce` names to the batch `run_id` on `chapter`, which
    /// needs `count` of its regions. Refused for another chapter's grant, one
    /// another batch claimed, and one that covers fewer.
    pub(crate) fn claim(&self, cloud_allowed: bool, nonce: &str, chapter: &str, run_id: &str, count: usize)
        -> Result<(), &'static str> {
        if !cloud_allowed {
            return Err(CLOUD_OFF);
        }
        let now = Instant::now();
        let mut live = self.lock();
        let Some(grant) = live.iter_mut().find(|grant| grant.nonce == nonce && !grant.lapsed(now)) else {
            return Err(NO_CONSENT);
        };
        if grant.chapter != chapter || grant.owner.is_some() || (grant.left as usize) < count {
            return Err(NO_CONSENT);
        }
        grant.owner = Some(run_id.to_owned());
        Ok(())
    }

    /// Withdraw the grant `nonce` names: Stop on the region it was confirmed
    /// for. Its render in flight ends.
    pub(crate) fn revoke(&self, nonce: &str) {
        Self::withdraw(&mut self.lock(), |grant| grant.nonce == nonce);
    }

    /// Withdraw the grant the batch `run_id` claimed: its Stop, or its end.
    pub(crate) fn revoke_owned_by(&self, run_id: &str) {
        Self::withdraw(&mut self.lock(), |grant| grant.owner.as_deref() == Some(run_id));
    }

    /// Withdraw every grant.
    pub(crate) fn revoke_every(&self) {
        Self::withdraw(&mut self.lock(), |_| true);
    }

    /// How many single-region renders hold a share right now, for the quit
    /// guard: a batch is counted as its run.
    pub(crate) fn single_renders(&self) -> usize {
        self.lock().iter().filter(|grant| grant.owner.is_none()).map(|grant| grant.spending as usize).sum()
    }
}

static BOOK: Book = Book::new(IDLE);

/// The process's grants.
pub(crate) fn book() -> &'static Book {
    &BOOK
}

/// Withdraw every grant: Cloud engines was turned off, or the app is quitting.
/// Every render in flight ends.
pub(crate) fn revoke_all() {
    BOOK.revoke_every();
}

/// What a confirmed window was granted.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NanoBananaGrant {
    pub grant: String,
    pub model: String,
    pub count: u32,
    /// The name of the profile whose key pays.
    pub profile: Option<String>,
}

/// Whether the settings now are the ones the window showed: its model, and
/// its profile when it named one.
fn as_shown(config: &Config, model: &str, profile_id: Option<&str>) -> bool {
    config.model.id == model && profile_id.is_none_or(|id| config.profile.as_ref().is_some_and(|profile| profile.id == id))
}

/// Make the grant for a window the person just confirmed: `count` regions of
/// `chapter_id` on `model` and the profile `profile_id`, the ones the window
/// showed. Refused while Cloud engines is off, and when settings name another
/// model or another active profile than the person saw.
#[tauri::command]
pub async fn nano_banana_grant(app: tauri::AppHandle, model: String, count: u32, chapter_id: String,
    profile_id: Option<String>) -> Result<NanoBananaGrant, String> {
    let config = Config::when_allowed(&app).ok_or(CLOUD_OFF)?;
    if !as_shown(&config, &model, profile_id.as_deref()) {
        return Err(NO_CONSENT.into());
    }
    let model = config.model.id.to_owned();
    let profile = config.profile.as_ref().map(|profile| profile.name.clone());
    let grant = book().issue(config, &chapter_id, count)?;
    Ok(NanoBananaGrant { grant, model, count, profile })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cleaner_core::engines::nano_banana::Model;

    fn config(instruction: Option<&str>) -> Config {
        Config { model: Model::DEFAULT, instruction: instruction.map(str::to_owned), restrictive: false, profile: None }
    }

    /// A single region's share, on chapter `c1`.
    fn click<'a>(book: &'a Book, nonce: &str) -> Result<Spend<'a>, &'static str> {
        book.spend(true, Some(nonce), "c1", None)
    }

    #[test]
    fn a_grant_covers_as_many_renders_as_were_confirmed() {
        let book = Book::new(IDLE);
        let nonce = book.issue(config(None), "c1", 2).unwrap();
        drop(click(&book, &nonce).unwrap());
        drop(click(&book, &nonce).unwrap());
        assert_eq!(click(&book, &nonce).err(), Some(NO_CONSENT));
    }

    #[test]
    fn nothing_is_taken_while_cloud_engines_are_off() {
        let book = Book::new(IDLE);
        let nonce = book.issue(config(None), "c1", 1).unwrap();
        assert_eq!(book.spend(false, Some(&nonce), "c1", None).err(), Some(CLOUD_OFF));
        assert!(click(&book, &nonce).is_ok(), "the refusal left the count alone");
    }

    #[test]
    fn no_grant_a_wrong_one_or_a_withdrawn_one_is_refused() {
        let book = Book::new(IDLE);
        assert_eq!(book.spend(true, None, "c1", None).err(), Some(NO_CONSENT));
        let nonce = book.issue(config(None), "c1", 5).unwrap();
        assert_eq!(click(&book, "0".repeat(64).as_str()).err(), Some(NO_CONSENT));
        book.revoke(&nonce);
        assert_eq!(click(&book, &nonce).err(), Some(NO_CONSENT));
    }

    #[test]
    fn a_grant_is_spent_only_on_its_own_chapter() {
        let book = Book::new(IDLE);
        let nonce = book.issue(config(None), "c1", 2).unwrap();
        assert_eq!(book.spend(true, Some(&nonce), "c2", None).err(), Some(NO_CONSENT));
        assert_eq!(book.claim(true, &nonce, "c2", "run-1", 1), Err(NO_CONSENT));
        assert_eq!(book.left(true, &nonce), Ok(2), "nothing was taken");
    }

    #[test]
    fn a_new_window_leaves_the_last_ones_grant_alone() {
        let book = Book::new(IDLE);
        let first = book.issue(config(None), "c1", 5).unwrap();
        let running = click(&book, &first).unwrap();
        let second = book.issue(config(None), "c2", 1).unwrap();
        assert!(!running.stop.raised(), "the first window's render goes on");
        assert!(click(&book, &first).is_ok());
        assert!(book.spend(true, Some(&second), "c2", None).is_ok());
    }

    #[test]
    fn a_grant_lapses_when_idle_but_not_while_a_render_runs() {
        let book = Book::new(Duration::from_millis(60));
        let nonce = book.issue(config(None), "c1", 3).unwrap();
        let running = click(&book, &nonce).unwrap();
        std::thread::sleep(Duration::from_millis(90));
        // A long region asks between its retries: the grant still holds.
        assert_eq!(book.left(true, &nonce), Ok(2));
        // The render ends now, and the clock starts again from here.
        drop(running);
        assert!(click(&book, &nonce).is_ok());
        std::thread::sleep(Duration::from_millis(90));
        assert_eq!(click(&book, &nonce).err(), Some(NO_CONSENT));
    }

    #[test]
    fn a_render_uses_the_instruction_that_was_confirmed() {
        let book = Book::new(IDLE);
        let nonce = book.issue(config(Some("keep the halftone")), "c1", 2).unwrap();
        let spend = click(&book, &nonce).unwrap();
        assert_eq!(spend.config.instruction.as_deref(), Some("keep the halftone"));
        assert_eq!(spend.config.model.id, Model::DEFAULT.id);
        assert_eq!(spend.granted().config.instruction.as_deref(), Some("keep the halftone"));
    }

    #[test]
    fn what_is_left_is_read_without_spending() {
        let book = Book::new(IDLE);
        let nonce = book.issue(config(None), "c1", 2).unwrap();
        assert_eq!(book.left(true, &nonce), Ok(2));
        assert_eq!(book.left(true, &nonce), Ok(2), "asking spends nothing");
        let spend = click(&book, &nonce).unwrap();
        assert_eq!(book.left(true, &nonce), Ok(1));
        drop(spend);
        assert_eq!(book.left(false, &nonce), Err(CLOUD_OFF));
        assert_eq!(book.left(true, "other"), Err(NO_CONSENT));
        book.revoke(&nonce);
        assert_eq!(book.left(true, &nonce), Err(NO_CONSENT));
    }

    /// **A grant binds the profile the window showed.** Another active
    /// profile, or none, is no grant; the grant names the profile it pays with.
    #[test]
    fn profiles_a_grant_binds_the_profile_the_window_showed() {
        use crate::inference::nano_banana_profiles::Chosen;
        let mut config = config(None);
        assert!(as_shown(&config, Model::DEFAULT.id, None), "a caller that names none");
        assert!(!as_shown(&config, Model::DEFAULT.id, Some("pa")), "no profile is active");
        config.profile = Some(Chosen { id: "pa".into(), name: "Work".into() });
        assert!(as_shown(&config, Model::DEFAULT.id, Some("pa")));
        assert!(!as_shown(&config, Model::DEFAULT.id, Some("pb")), "another profile became active");
        assert!(!as_shown(&config, "gemini-3-pro-image", Some("pa")), "another model");
        let book = Book::new(IDLE);
        let nonce = book.issue(config, "c1", 1).unwrap();
        assert_eq!(book.profile_of(&nonce).as_deref(), Some("Work"));
    }

    #[test]
    fn a_count_of_none_or_too_many_is_no_grant() {
        let book = Book::new(IDLE);
        assert_eq!(book.issue(config(None), "c1", 0).err(), Some(NO_CONSENT));
        assert_eq!(book.issue(config(None), "c1", MAX_REGIONS + 1).err(), Some(NO_CONSENT));
    }

    #[test]
    fn a_stop_after_the_share_is_spent_reaches_the_render() {
        let book = Book::new(IDLE);
        let nonce = book.issue(config(None), "c1", 1).unwrap();
        let spend = click(&book, &nonce).unwrap();
        assert!(!spend.stop.raised());
        book.revoke(&nonce);
        assert!(spend.stop.raised(), "the render that already holds its share is stopped");
    }

    #[test]
    fn a_claimed_grant_is_that_batchs_alone_and_its_stop_is_its_own() {
        let book = Book::new(IDLE);
        let batch = book.issue(config(None), "c1", 3).unwrap();
        let single = book.issue(config(None), "c1", 1).unwrap();
        assert_eq!(book.claim(true, &batch, "c1", "run-1", 4), Err(NO_CONSENT), "covers fewer");
        assert_eq!(book.claim(true, &batch, "c1", "run-1", 3), Ok(()));
        assert_eq!(book.claim(true, &batch, "c1", "run-2", 1), Err(NO_CONSENT), "claimed once");
        assert_eq!(click(&book, &batch).err(), Some(NO_CONSENT), "a single region can not spend a batch's grant");
        let region = book.spend(true, Some(&batch), "c1", Some("run-1")).unwrap();
        let other = click(&book, &single).unwrap();
        assert_eq!(book.single_renders(), 1, "the batch is counted as its run");

        // Stop on the single region leaves the batch alone, and the other way round.
        book.revoke(&single);
        assert!(other.stop.raised() && !region.stop.raised());
        book.revoke_owned_by("run-1");
        assert!(region.stop.raised());
        assert_eq!(book.left(true, &batch), Err(NO_CONSENT));
    }

    #[test]
    fn turning_cloud_engines_off_stops_every_render() {
        let book = Book::new(IDLE);
        let a = book.issue(config(None), "c1", 1).unwrap();
        let b = book.issue(config(None), "c2", 1).unwrap();
        let (first, second) = (click(&book, &a).unwrap(), book.spend(true, Some(&b), "c2", None).unwrap());
        book.revoke_every();
        assert!(first.stop.raised() && second.stop.raised());
    }
}
