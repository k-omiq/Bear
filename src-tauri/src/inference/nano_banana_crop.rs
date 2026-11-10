//! The crop shown to the person before Nano Banana sends it.
//!
//! With Settings > Cloud > Nano Banana > **Show each crop before it is sent**
//! on (`nanoBananaShowCrop`, read at each region's edit), every crop waits
//! here before it goes to Google: the render emits a `nano-banana-crop`
//! event with the picture and waits for [`nano_banana_crop_answer`]. The
//! person sends it, skips the region, or stops the request. With
//! **Restrictive crop** on, the event carries the areas the app suggests
//! keeping, and the person may answer with areas of their own; the native
//! side keeps the marked regions whatever they pick.
//!
//! Only one Nano Banana edit holds the gate at a time, and a crop is shown
//! while its edit holds it, so at most one crop waits at once. A crop waits
//! until it is answered, the edit is stopped (its grant withdrawn, cloud
//! engines off, quit), or the edit's deadline passes. A crop with no window to
//! show it in is not sent.

use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Duration;

use cleaner_core::mask::Rect;

use crate::events::Event;

use super::nano_banana::Failure;

/// The settings key: whether each crop is shown before it is sent.
pub const SHOW_KEY: &str = "nanoBananaShowCrop";

/// The answer for an id no crop is waiting under: it was answered, or its
/// edit ended meanwhile.
pub const GONE: &str = "nanoBanana.crop.gone";
/// An answer the window could not have sent.
pub const BAD_ANSWER: &str = "nanoBanana.crop.badAnswer";

/// The most areas one answer may keep.
const MAX_AREAS: usize = 64;
/// The longest side an area may have: the largest crop's.
const MAX_SIDE: u32 = 8192;
const TICK: Duration = Duration::from_millis(100);

/// What the person answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Answer {
    /// Send it. With restrictive crops on, the areas to keep in crop pixels,
    /// or `None` for the ones suggested.
    Send(Option<Vec<Rect>>),
    /// Do not send it: the region is not cleaned.
    Skip,
    /// End the request and its grant.
    Stop,
}

/// What one crop is shown as. Every rectangle is `[x, y, w, h]`.
pub(crate) struct Shown {
    pub model: &'static str,
    pub ratio: &'static str,
    pub size: &'static str,
    /// Where the crop is on the page, in page pixels.
    pub crop: [i64; 4],
    /// How many crops of this region Google refused before this one.
    pub attempt: usize,
    pub restrictive: bool,
    /// In crop pixels: the bounds of each region marked in the crop.
    pub holes: Vec<[i64; 4]>,
    /// In crop pixels: the areas kept unless the person picks others.
    pub suggested: Vec<[i64; 4]>,
    /// The crop as marked, nothing made white, as PNG.
    pub png: Vec<u8>,
}

struct Waiting {
    id: String,
    answer: Option<Answer>,
}

struct Desk {
    waiting: Mutex<Vec<Waiting>>,
    answered: Condvar,
}

static DESK: Desk = Desk { waiting: Mutex::new(Vec::new()), answered: Condvar::new() };

fn new_id() -> Result<String, Failure> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| Failure::NoWindow)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Show `shown` and wait for the answer. `still` is the edit's own check (its
/// stop and deadline), asked every tick: its failure ends the wait.
pub(crate) fn ask(shown: Shown, still: &dyn Fn() -> Result<(), Failure>) -> Result<Answer, Failure> {
    if !crate::events::subscribed() {
        return Err(Failure::NoWindow);
    }
    let id = new_id()?;
    let event = Event::NanoBananaCrop {
        id: id.clone(),
        model: shown.model.to_owned(),
        ratio: shown.ratio.to_owned(),
        size: shown.size.to_owned(),
        crop: shown.crop,
        attempt: shown.attempt,
        restrictive: shown.restrictive,
        holes: shown.holes,
        suggested: shown.suggested,
        png: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &shown.png),
    };
    DESK.waiting.lock().unwrap_or_else(PoisonError::into_inner).push(Waiting { id: id.clone(), answer: None });
    crate::events::emit(&event);
    let mut waiting = DESK.waiting.lock().unwrap_or_else(PoisonError::into_inner);
    let outcome = loop {
        let at = waiting.iter().position(|entry| entry.id == id);
        if let Some(answer) = at.and_then(|at| waiting[at].answer.clone()) {
            break Ok(answer);
        }
        if let Err(failure) = still() {
            break Err(failure);
        }
        waiting = DESK.answered.wait_timeout(waiting, TICK).unwrap_or_else(PoisonError::into_inner).0;
    };
    waiting.retain(|entry| entry.id != id);
    outcome
}

/// One area of an answer, in crop pixels.
fn area(raw: [i64; 4]) -> Option<Rect> {
    let [x, y, w, h] = raw;
    let side = |v: i64| u32::try_from(v).ok().filter(|v| (1..=MAX_SIDE).contains(v));
    let bound = i64::from(MAX_SIDE);
    ((-bound..=bound).contains(&x) && (-bound..=bound).contains(&y)).then_some(())?;
    Some(Rect::new(x, y, side(w)?, side(h)?))
}

/// Read what the window sent.
fn read(answer: &str, keep: Option<Vec<[i64; 4]>>) -> Result<Answer, &'static str> {
    match answer {
        "send" => match keep {
            None => Ok(Answer::Send(None)),
            Some(areas) if areas.len() <= MAX_AREAS => {
                let areas: Option<Vec<Rect>> = areas.into_iter().map(area).collect();
                Ok(Answer::Send(Some(areas.ok_or(BAD_ANSWER)?)))
            }
            Some(_) => Err(BAD_ANSWER),
        },
        "skip" => Ok(Answer::Skip),
        "stop" => Ok(Answer::Stop),
        _ => Err(BAD_ANSWER),
    }
}

/// Hand `answer` to the crop waiting under `id`.
fn deliver(id: &str, answer: Answer) -> Result<(), &'static str> {
    let mut waiting = DESK.waiting.lock().unwrap_or_else(PoisonError::into_inner);
    let entry = waiting.iter_mut().find(|entry| entry.id == id && entry.answer.is_none()).ok_or(GONE)?;
    entry.answer = Some(answer);
    DESK.answered.notify_all();
    Ok(())
}

/// `answerNanoBananaCrop`: the person's answer to a crop shown before it is
/// sent. `answer` is `send`, `skip` or `stop`; `keep` is the areas to keep,
/// `[x, y, w, h]` in crop pixels, read only for a restrictive crop, `null`
/// for the suggested ones. Refused with [`GONE`] when that crop is no longer
/// waiting.
#[tauri::command]
pub fn nano_banana_crop_answer(id: String, answer: String, keep: Option<Vec<[i64; 4]>>) -> Result<(), String> {
    let answer = read(&answer, keep)?;
    deliver(&id, answer).map_err(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_is_read_as_the_window_meant_it() {
        assert_eq!(read("send", None), Ok(Answer::Send(None)));
        assert_eq!(read("send", Some(vec![[1, 2, 3, 4]])), Ok(Answer::Send(Some(vec![Rect::new(1, 2, 3, 4)]))));
        assert_eq!(read("skip", Some(vec![[1, 2, 3, 4]])), Ok(Answer::Skip));
        assert_eq!(read("stop", None), Ok(Answer::Stop));
        for bad in [read("go", None), read("send", Some(vec![[0, 0, 0, 4]])), read("send", Some(vec![[0, 0, -1, 4]])),
            read("send", Some(vec![[0, 0, 9000, 4]])), read("send", Some(vec![[i64::MIN, 0, 1, 1]])),
            read("send", Some(vec![[0, 0, 1, 1]; MAX_AREAS + 1]))] {
            assert_eq!(bad, Err(BAD_ANSWER));
        }
    }

    #[test]
    fn an_answer_for_no_waiting_crop_is_gone() {
        assert_eq!(deliver("nobody", Answer::Skip), Err(GONE));
    }

    #[test]
    fn a_stopped_edit_stops_waiting() {
        // A sink so the crop has somewhere to be shown; it answers nothing.
        let sink = crate::events::register(Box::new(|_| {}));
        let shown = Shown {
            model: "m", ratio: "1:1", size: "512", crop: [0, 0, 8, 8], attempt: 0, restrictive: false,
            holes: Vec::new(), suggested: Vec::new(), png: Vec::new(),
        };
        let asked = ask(shown, &|| Err(Failure::Stopped));
        crate::events::unregister(sink);
        assert_eq!(asked, Err(Failure::Stopped));
    }
}
