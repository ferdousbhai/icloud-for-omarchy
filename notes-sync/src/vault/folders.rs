//! Folder reconciliation on pull and folder creation on push. Originally
//! derived from icloud-md.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use indexmap::{IndexMap, IndexSet};

use super::base::{read_base_copy, read_sent_copy, replace_base_copy, write_sent_copy};
use super::layout::{
    RESERVED_SIBLING_DIR_NAMES, RESERVED_TOP_LEVEL_DIR_NAMES, StateDirInfo, VaultLayout, expected_note_dir, note_dir_of,
};
use super::local::{read_text, remove_stale_temporaries, rename_no_replace, write_note_file};
use super::pairing::claim_names_on_disk;
use super::state::{AttachmentEntry, NoteEntry};
use crate::cmd::errors::Error;
use crate::cmd::plan::FolderRefusal;
use crate::cmd::used_names_for;
use crate::doc::embeds::{
    AttachmentReference, attachment_link_spellings, format_attachment_markdown, has_attachment_link_to,
};
use crate::js::{self, posix};
use crate::md::filename::unique_file_name;

// --- folder reconciliation (pull) ----------------------------------------------

/// `Relocation`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relocation {
    pub from: String,
    pub to: String,
}

/// Moves the file `from` into the directory `to_dir` (both
/// vault-relative; the directory is made if need be) under `name`, or under
/// the next `" 2"` name when that one is in `used` or turns out to be taken
/// on disk: nothing there is ever replaced. The name it got, which joins
/// `used`; `None` when there is no file at `from`. `naming` is told each
/// name before the file is renamed to it, and `name` again when there turns
/// out to be no file to move.
fn move_to_free_name(
    target_dir: &Path,
    from: &str,
    to_dir: &str,
    name: &str,
    used: &mut HashSet<String>,
    naming: &mut dyn FnMut(&str) -> Result<(), Error>,
) -> Result<Option<String>, Error> {
    let from = target_dir.join(from);
    // Nothing to move makes no directory: one made empty for a note
    // deleted here would bring back a folder directory deleted here too.
    if matches!(std::fs::symlink_metadata(&from), Err(e) if e.kind() == std::io::ErrorKind::NotFound) {
        return Ok(None);
    }
    std::fs::create_dir_all(target_dir.join(to_dir))?;
    loop {
        let candidate = unique_file_name(name, used);
        used.insert(candidate.clone());
        naming(&candidate)?;
        match rename_no_replace(&from, &target_dir.join(to_dir).join(&candidate)) {
            Ok(()) => return Ok(Some(candidate)),
            // Taken since the names on disk were read: the next name.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                used.remove(&candidate);
                naming(name)?;
                return Ok(None);
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// `reconcileNotePlacements`: move every tracked note (and its attachments)
/// whose directory no longer matches the layout. Remote wins.
pub fn reconcile_note_placements(
    target_dir: &Path,
    layout: &VaultLayout,
    notes: &mut IndexMap<String, NoteEntry>,
    attachments: &mut IndexMap<String, AttachmentEntry>,
) -> Result<Vec<Relocation>, Error> {
    let mut moves: Vec<(String, String)> = Vec::new();
    let mut claimed_names: HashMap<String, HashSet<String>> = HashMap::new();
    let mut claimed_attachment_names: HashMap<String, HashSet<String>> = HashMap::new();

    let mut sorted: Vec<(&String, &NoteEntry)> = notes.iter().collect();
    sorted.sort_by(|(a, _), (b, _)| js::locale_compare(a, b));
    for (record_name, entry) in sorted {
        let to_dir = expected_note_dir(layout, entry);
        match to_dir {
            Some(to_dir) if to_dir != note_dir_of(&entry.file) => moves.push((record_name.clone(), to_dir)),
            _ => {
                used_names_for(&mut claimed_names, &note_dir_of(&entry.file))
                    .insert(posix::basename(&entry.file).to_owned());
            }
        }
    }
    let moving: HashSet<&str> = moves.iter().map(|(rn, _)| rn.as_str()).collect();
    for attachment in attachments.values() {
        if !moving.contains(attachment.note_record_name.as_str()) {
            used_names_for(
                &mut claimed_attachment_names,
                &note_dir_of(&posix::dirname(&attachment.file)),
            )
            .insert(posix::basename(&attachment.file).to_owned());
        }
    }

    let mut relocations = Vec::new();
    for (record_name, to_dir) in &moves {
        let Some(entry) = notes.get(record_name) else {
            continue;
        };
        let from_file = entry.file.clone();
        claim_names_on_disk(target_dir, to_dir, used_names_for(&mut claimed_names, to_dir))?;
        let Some(base_name) = move_to_free_name(
            target_dir,
            &from_file,
            to_dir,
            posix::basename(&from_file),
            used_names_for(&mut claimed_names, to_dir),
            &mut |_| Ok(()),
        )?
        else {
            used_names_for(&mut claimed_names, &note_dir_of(&from_file)).insert(posix::basename(&from_file).to_owned());
            continue;
        };
        let to_file = posix::join(&[to_dir, &base_name]);
        notes[record_name].file = to_file.clone();
        relocations.push(Relocation {
            from: from_file,
            to: to_file.clone(),
        });

        let attachment_names: Vec<String> = attachments
            .iter()
            .filter(|(_, a)| a.note_record_name == *record_name)
            .map(|(k, _)| k.clone())
            .collect();
        let to_attachments = posix::join(&[to_dir, "attachments"]);
        if !attachment_names.is_empty() {
            // What is in the target's `attachments/` stays: a file no note
            // tracks (put there to be attached), or one of a note that is
            // itself still to move out.
            claim_names_on_disk(
                target_dir,
                &to_attachments,
                used_names_for(&mut claimed_attachment_names, to_dir),
            )?;
        }
        for attachment_record_name in attachment_names {
            let attachment = attachments[&attachment_record_name].clone();
            let old_base = posix::basename(&attachment.file).to_owned();
            let Some(new_base) = move_to_free_name(
                target_dir,
                &attachment.file,
                &to_attachments,
                &old_base,
                used_names_for(&mut claimed_attachment_names, to_dir),
                &mut links_follow(target_dir, &to_file, record_name, &old_base, attachments)?,
            )?
            else {
                continue;
            };
            attachments[&attachment_record_name].file = posix::join(&[&to_attachments, &new_base]);
        }
    }
    Ok(relocations)
}

/// Moves a note's attachment files into `attachments/` beside the note's
/// (new) file, after the note itself was moved there. A name taken there
/// gets a `" 2"` name, and the note's links (and base copy) follow it.
pub fn relocate_note_attachments(
    target_dir: &Path,
    record_name: &str,
    note_file: &str,
    attachments: &mut IndexMap<String, AttachmentEntry>,
) -> Result<(), Error> {
    relocate_note_attachments_among(target_dir, record_name, note_file, attachments, &mut HashSet::new())
}

/// [`relocate_note_attachments`] where `used` holds the names already given
/// in that `attachments/` that may not be on disk yet (a pull's downloads
/// still in flight, which land there by a rename that replaces); the names
/// the attachments end up with join it.
pub fn relocate_note_attachments_among(
    target_dir: &Path,
    record_name: &str,
    note_file: &str,
    attachments: &mut IndexMap<String, AttachmentEntry>,
    used: &mut HashSet<String>,
) -> Result<(), Error> {
    let to_dir = note_dir_of(note_file);
    let to_attachments = posix::join(&[&to_dir, "attachments"]);
    claim_names_on_disk(target_dir, &to_attachments, used)?;
    let mine: Vec<String> = attachments
        .iter()
        .filter(|(_, a)| a.note_record_name == record_name && note_dir_of(&posix::dirname(&a.file)) != to_dir)
        .map(|(k, _)| k.clone())
        .collect();
    for attachment_record_name in mine {
        let attachment = attachments[&attachment_record_name].clone();
        let old_base = posix::basename(&attachment.file).to_owned();
        // Moved along with a renamed folder directory, or by a run cut short
        // after the move: already there, under its name or the free name the
        // note's file links (the links follow before the file moves). Never
        // a file of that name there that is another attachment's.
        if !target_dir.join(&attachment.file).exists()
            && let Some(already_there) = already_moved(target_dir, note_file, &to_attachments, &old_base, attachments)?
        {
            used.insert(posix::basename(&already_there).to_owned());
            attachments[&attachment_record_name].file = already_there;
            continue;
        }
        let Some(new_base) = move_to_free_name(
            target_dir,
            &attachment.file,
            &to_attachments,
            &old_base,
            used,
            &mut links_follow(target_dir, note_file, record_name, &old_base, attachments)?,
        )?
        else {
            continue;
        };
        attachments[&attachment_record_name].file = posix::join(&[&to_attachments, &new_base]);
    }
    Ok(())
}

/// Where in `to_attachments` an attachment called `old_base` already is,
/// when its file is gone from where it was tracked: the first of its name
/// and the free names after it (`name 2.ext`, ...) that `note_file` links
/// and no other attachment tracks, else its own name when no other
/// attachment tracks that.
fn already_moved(
    target_dir: &Path,
    note_file: &str,
    to_attachments: &str,
    old_base: &str,
    attachments: &IndexMap<String, AttachmentEntry>,
) -> Result<Option<String>, Error> {
    let text = read_text(&target_dir.join(note_file))?.unwrap_or_default();
    let untracked = |file: &str| target_dir.join(file).exists() && !attachments.values().any(|a| a.file == file);
    let mut names = HashSet::new();
    loop {
        let name = unique_file_name(old_base, &names);
        let file = posix::join(&[to_attachments, &name]);
        if !target_dir.join(&file).exists() {
            break;
        }
        if untracked(&file) && has_attachment_link_to(&text, &format!("attachments/{name}")) {
            return Ok(Some(file));
        }
        names.insert(name);
    }
    let own_name = posix::join(&[to_attachments, old_base]);
    Ok(untracked(&own_name).then_some(own_name))
}

/// For [`move_to_free_name`]: the links to an attachment file in
/// `note_file` and its base copy follow each name the file is about to get,
/// so that a run cut short after the move finds the file by its link. They
/// follow from the name the file links now: its own, or, when a run cut
/// short before the move left them on a free name (`name 2.ext`) that is
/// taken since, that one (none of the note's other attachments' names).
fn links_follow<'a>(
    target_dir: &'a Path,
    note_file: &'a str,
    record_name: &'a str,
    old_base: &str,
    attachments: &IndexMap<String, AttachmentEntry>,
) -> Result<impl FnMut(&str) -> Result<(), Error> + 'a, Error> {
    let text = read_text(&target_dir.join(note_file))?.unwrap_or_default();
    let links = |name: &str| has_attachment_link_to(&text, &format!("attachments/{name}"));
    let mut linked_as = vec![old_base.to_owned()];
    if !links(old_base) {
        let siblings: HashSet<&str> = attachments
            .values()
            .filter(|a| a.note_record_name == record_name)
            .map(|a| posix::basename(&a.file))
            .collect();
        let mut names = HashSet::from([old_base.to_owned()]);
        for _ in 0..100 {
            let name = unique_file_name(old_base, &names);
            if links(&name) && !siblings.contains(name.as_str()) {
                // The base copy may still link the old name.
                linked_as.push(name);
                break;
            }
            names.insert(name);
        }
    }
    Ok(move |name: &str| {
        if linked_as.iter().any(|from| from != name) {
            for from in linked_as.iter().filter(|from| *from != name) {
                rewrite_attachment_link(target_dir, note_file, record_name, from, name)?;
            }
            linked_as = vec![name.to_owned()];
        }
        Ok(())
    })
}

/// `removeStaleDirs`: best-effort rmdir of directories the previous layout
/// used and the current one doesn't (deepest first; only if empty). A
/// temporary file a killed run left in one is no reason to keep it: kept,
/// the next push would make the directory a folder in iCloud again.
pub fn remove_stale_dirs(target_dir: &Path, previous_dirs: &[String], current_dirs: &HashSet<String>) {
    let mut stale: Vec<&String> = previous_dirs
        .iter()
        .filter(|d| !d.is_empty() && !current_dirs.contains(*d))
        .collect();
    stale.sort_by_key(|d| std::cmp::Reverse(d.split('/').count()));
    for dir in stale {
        let dir = target_dir.join(dir);
        remove_stale_temporaries(&dir.join("attachments"), true);
        let _ = std::fs::remove_dir(dir.join("attachments"));
        remove_stale_temporaries(&dir, false);
        let _ = std::fs::remove_dir(dir);
    }
}

fn rewrite_attachment_link(
    target_dir: &Path,
    note_file: &str,
    record_name: &str,
    old_base: &str,
    new_base: &str,
) -> Result<(), Error> {
    // A link as pull writes it (either spelling of its text) becomes the
    // link pull would write for the new name, text and all: that is the
    // one push takes for the attachment. Any other link to the file keeps
    // its text and gets the new target. Whole links only: `x.png` renamed
    // leaves a link to `x.png.pdf` alone.
    let file = AttachmentReference {
        attachment_identifier: String::new(),
        type_uti: "public.data".into(),
    };
    let link_to = |base: &str| format!("attachments/{base}");
    let new_link = format_attachment_markdown(&file, &link_to(new_base));
    let target_of = |link: &str| link[link.find("](").unwrap_or(0)..].to_owned();
    let rewrite = |text: &str| -> String {
        let mut text = text.to_owned();
        for old_link in attachment_link_spellings(&file, &link_to(old_base)) {
            text = text.replace(&old_link, &new_link);
            let old_target = target_of(&old_link);
            text = text.replace(&old_target, &target_of(&new_link));
            // As it was written before parentheses that don't pair up
            // were percent-encoded.
            let bare = old_target.replace("%28", "(").replace("%29", ")");
            text = text.replace(&bare, &target_of(&new_link));
        }
        text
    };
    let note_path = target_dir.join(note_file);
    if let Some(content) = read_text(&note_path)? {
        write_note_file(&note_path, &rewrite(&content))?;
    }
    // The base copy, and the text a push cut short sent (which iCloud may
    // have, and now reads with the new link): rewritten as they are, the
    // sent copy kept.
    if let Some(base) = read_base_copy(target_dir, record_name)? {
        replace_base_copy(target_dir, record_name, &rewrite(&base))?;
    }
    if let Some(sent) = read_sent_copy(target_dir, record_name)? {
        write_sent_copy(target_dir, record_name, &rewrite(&sent))?;
    }
    Ok(())
}

// --- folder creation (push) ----------------------------------------------------

/// `PlannedFolder`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFolder {
    pub record_name: String,
    pub title: String,
    pub dir_path: String,
    pub parent_record_name: Option<String>,
}

/// `RefusedFolder`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedFolder {
    pub dir_path: String,
    pub reason: FolderRefusal,
}

/// `FolderCreatePlan`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderCreatePlan {
    /// Parent before child.
    pub folders: Vec<PlannedFolder>,
    pub refusals: Vec<RefusedFolder>,
    /// Directory → folder record name, planned or existing.
    pub dir_to_record_name: IndexMap<String, String>,
}

fn reserved_reason(segment: &str, is_top_level: bool) -> Option<FolderRefusal> {
    if segment.starts_with('.') {
        return Some(FolderRefusal::Hidden {
            segment: segment.to_owned(),
        });
    }
    if RESERVED_SIBLING_DIR_NAMES.contains(&segment) {
        return Some(FolderRefusal::ReservedAttachments {
            segment: segment.to_owned(),
        });
    }
    if is_top_level && RESERVED_TOP_LEVEL_DIR_NAMES.contains(&segment) {
        return Some(FolderRefusal::ReservedStateDir {
            segment: segment.to_owned(),
        });
    }
    None
}

/// `planFolderCreates`: which directories a note is going into must become
/// Folder records, walked root-down; nothing inside a sharer's area.
pub fn plan_folder_creates<'a>(
    wanted_dirs: impl IntoIterator<Item = &'a str>,
    dir_index: &IndexMap<String, StateDirInfo>,
    new_record_name: &mut dyn FnMut() -> String,
) -> FolderCreatePlan {
    let mut plan = FolderCreatePlan::default();
    let mut refused_dirs: HashSet<String> = HashSet::new();
    for (dir, info) in dir_index {
        if let StateDirInfo::Folder { folder_record_name, .. } = info {
            plan.dir_to_record_name.insert(dir.clone(), folder_record_name.clone());
        }
    }

    let unique: IndexSet<&str> = wanted_dirs.into_iter().collect();
    let mut sorted: Vec<String> = unique
        .into_iter()
        .filter(|d| !d.is_empty())
        .map(str::to_owned)
        .collect();
    sorted.sort_by_key(|d| d.split('/').count());

    for dir in &sorted {
        if plan.dir_to_record_name.contains_key(dir) || refused_dirs.contains(dir) {
            continue;
        }
        let mut parent_record_name: Option<String> = None;
        let mut refusal: Option<FolderRefusal> = None;
        let mut walked = String::new();
        for (depth, segment) in dir.split('/').enumerate() {
            walked = if walked.is_empty() {
                segment.to_owned()
            } else {
                format!("{walked}/{segment}")
            };
            match dir_index.get(&walked) {
                Some(StateDirInfo::SharerHome { .. }) => {
                    refusal = Some(FolderRefusal::SharerHome { walked: walked.clone() });
                    break;
                }
                Some(StateDirInfo::Folder {
                    shared_zone_owner: Some(_),
                    ..
                }) => {
                    refusal = Some(FolderRefusal::SharedFolder { walked: walked.clone() });
                    break;
                }
                _ => {}
            }
            if let Some(known) = plan.dir_to_record_name.get(&walked) {
                parent_record_name = Some(known.clone());
                continue;
            }
            if let Some(reserved) = reserved_reason(segment, depth == 0) {
                refusal = Some(reserved);
                break;
            }
            let planned = PlannedFolder {
                record_name: new_record_name(),
                title: segment.to_owned(),
                dir_path: walked.clone(),
                parent_record_name: parent_record_name.clone(),
            };
            plan.dir_to_record_name
                .insert(walked.clone(), planned.record_name.clone());
            parent_record_name = Some(planned.record_name.clone());
            plan.folders.push(planned);
        }

        if let Some(reason) = refusal {
            plan.refusals.push(RefusedFolder {
                dir_path: dir.clone(),
                reason,
            });
            refused_dirs.insert(dir.clone());
            drop_unreachable(&mut plan, &sorted, &refused_dirs);
        }
    }
    plan
}

fn drop_unreachable(plan: &mut FolderCreatePlan, wanted: &[String], refused_dirs: &HashSet<String>) {
    let live: Vec<&String> = wanted.iter().filter(|d| !refused_dirs.contains(*d)).collect();
    let mut i = plan.folders.len();
    while i > 0 {
        i -= 1;
        let folder_dir = plan.folders[i].dir_path.clone();
        let needed = live
            .iter()
            .any(|d| **d == folder_dir || d.starts_with(&format!("{folder_dir}/")));
        if !needed {
            plan.folders.remove(i);
            plan.dir_to_record_name.shift_remove(&folder_dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A name taken on disk that `used` doesn't hold (a file delivered after
    /// the directory was read) is not replaced: the move takes the next name.
    #[test]
    fn a_move_never_replaces_a_file_that_is_not_among_the_used_names() {
        let dir = std::env::temp_dir().join(format!("icloud-notes-sync-move-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Alpha")).unwrap();
        std::fs::create_dir_all(dir.join("Beta")).unwrap();
        std::fs::write(dir.join("Alpha/Pie.md"), "moving").unwrap();
        std::fs::write(dir.join("Beta/Pie.md"), "delivered since").unwrap();

        let mut used = HashSet::new();
        let name = move_to_free_name(&dir, "Alpha/Pie.md", "Beta", "Pie.md", &mut used, &mut |_| Ok(())).unwrap();
        assert_eq!(name.as_deref(), Some("Pie 2.md"));
        assert_eq!(
            std::fs::read_to_string(dir.join("Beta/Pie.md")).unwrap(),
            "delivered since"
        );
        assert_eq!(std::fs::read_to_string(dir.join("Beta/Pie 2.md")).unwrap(), "moving");
        assert!(used.contains("Pie.md") && used.contains("Pie 2.md"));

        assert_eq!(
            move_to_free_name(&dir, "Alpha/Gone.md", "Beta", "Gone.md", &mut used, &mut |_| Ok(())).unwrap(),
            None
        );
        assert!(!used.contains("Gone.md"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    const NOTE: &str = "a1b2c3d4-0000-4000-8000-000000000001";

    /// A vault with the note moved here to `Beta/Pie.md` (linking `link`, its
    /// base copy linking `base_link`) and its photo tracked at
    /// `Alpha/attachments/x.png`, with `taken` files in `Beta/attachments/`.
    fn moved_note(
        name: &str,
        link: &str,
        base_link: &str,
        taken: &[&str],
    ) -> (PathBuf, IndexMap<String, AttachmentEntry>) {
        let dir = std::env::temp_dir().join(format!("icloud-notes-sync-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Alpha/attachments")).unwrap();
        std::fs::create_dir_all(dir.join("Beta/attachments")).unwrap();
        std::fs::write(dir.join("Alpha/attachments/x.png"), "the photo").unwrap();
        for file in taken {
            std::fs::write(dir.join("Beta/attachments").join(file), "another file").unwrap();
        }
        let text = |link: &str| format!("Pie\n![x.png](attachments/{})\n", link.replace(' ', "%20"));
        std::fs::write(
            dir.join("Beta/Pie.md"),
            format!("---\napple-note-id: {NOTE}\n---\n{}", text(link)),
        )
        .unwrap();
        super::super::base::write_base_copy(&dir, NOTE, &text(base_link)).unwrap();
        let mut attachments = IndexMap::new();
        attachments.insert(
            "ATT".to_owned(),
            AttachmentEntry {
                file: "Alpha/attachments/x.png".into(),
                media_record_name: "MEDIA".into(),
                media_file_checksum: "ck".into(),
                note_record_name: NOTE.into(),
            },
        );
        (dir, attachments)
    }

    /// A move to a free name keeps the text a push cut short sent for the
    /// note (`.sent`), its link following the file like the base copy's.
    #[test]
    fn a_free_name_keeps_the_sent_copy_with_its_link_following() {
        let (dir, mut attachments) = moved_note("sent", "x.png", "x.png", &["x.png"]);
        let sent = "Pie\n![x.png](attachments/x.png)\nsent\n";
        super::super::base::write_sent_copy(&dir, NOTE, sent).unwrap();
        relocate_note_attachments(&dir, NOTE, "Beta/Pie.md", &mut attachments).unwrap();
        assert_eq!(attachments["ATT"].file, "Beta/attachments/x 2.png");
        let sent = super::super::base::read_sent_copy(&dir, NOTE)
            .unwrap()
            .expect("the sent copy is kept");
        assert!(sent.contains("](attachments/x%202.png)"), "{sent}");
        assert!(sent.ends_with("sent\n"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A run cut short after the links followed `x 2.png` and before the
    /// file moved, with `x 2.png` taken there since: the file gets `x 3.png`
    /// and the links (in the file, and the base copy still on `x.png`)
    /// follow it from where each is.
    #[test]
    fn links_left_on_a_free_name_taken_since_follow_the_next_one() {
        let (dir, mut attachments) = moved_note("taken-since", "x 2.png", "x.png", &["x.png", "x 2.png"]);
        relocate_note_attachments(&dir, NOTE, "Beta/Pie.md", &mut attachments).unwrap();
        assert_eq!(attachments["ATT"].file, "Beta/attachments/x 3.png");
        assert_eq!(
            std::fs::read_to_string(dir.join("Beta/attachments/x 3.png")).unwrap(),
            "the photo"
        );
        let text = std::fs::read_to_string(dir.join("Beta/Pie.md")).unwrap();
        let base = super::super::base::read_base_copy(&dir, NOTE).unwrap().unwrap();
        for read in [&text, &base] {
            assert!(read.contains("](attachments/x%203.png)"), "{read}");
            assert!(
                !read.contains("](attachments/x%202.png)") && !read.contains("](attachments/x.png)"),
                "{read}"
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
