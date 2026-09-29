//! Ports of icloud-md's `frontmatter.test.ts` and `noteIdFrontmatter.test.ts`.

use icloud_notes_sync::md::frontmatter::*;

fn lossless(text: &str) -> Envelope {
    let split = split_frontmatter(text, SplitOptions::default());
    assert_eq!(
        join_frontmatter(&split.frontmatter, &split.body),
        text,
        "not lossless for {text:?}"
    );
    split
}

fn titled() -> SplitOptions {
    SplitOptions {
        filename_as_title: true,
    }
}

#[test]
fn split_cases() {
    let cases: [(&str, &str, &str); 10] = [
        ("# Title\nbody line", "", "# Title\nbody line"),
        (
            "---\ntags: [a, b]\n---\n# Title\nbody",
            "---\ntags: [a, b]\n---\n",
            "# Title\nbody",
        ),
        (
            "---\ntags: [a]\n---\n\n# Title\nbody",
            "---\ntags: [a]\n---\n\n",
            "# Title\nbody",
        ),
        ("---\nx: 1\n---\n\n\n# Title", "---\nx: 1\n---\n\n\n", "# Title"),
        ("---\nx: 1\n---", "---\nx: 1\n---", ""),
        ("---\nx: 1\n---\n", "---\nx: 1\n---\n", ""),
        ("# Title\n---\nmore", "", "# Title\n---\nmore"),
        (
            "---\nlooks like yaml\nbut never closes",
            "",
            "---\nlooks like yaml\nbut never closes",
        ),
        ("", "", ""),
        ("---\n---\n# Title", "---\n---\n", "# Title"),
    ];
    for (text, frontmatter, body) in cases {
        let split = lossless(text);
        assert_eq!(split.frontmatter, frontmatter, "{text:?}");
        assert_eq!(split.body, body, "{text:?}");
    }
}

#[test]
fn join_reattaches_a_preserved_envelope() {
    let split = split_frontmatter("---\ntags: [keep]\n---\n\n# Old Title\nold", SplitOptions::default());
    assert_eq!(
        join_frontmatter(&split.frontmatter, "# New Title\nnew body"),
        "---\ntags: [keep]\n---\n\n# New Title\nnew body"
    );
    assert_eq!(join_frontmatter("", "# Title"), "# Title");
}

#[test]
fn filename_as_title_bodies() {
    let text = "---\n\nSome prose\n\n---\n\nMore prose";
    let split = split_frontmatter(text, titled());
    assert_eq!(split.frontmatter, "");
    assert_eq!(split.body, text);

    let text = "---\n---\nprose";
    assert_eq!(
        split_frontmatter(text, titled()),
        Envelope {
            frontmatter: String::new(),
            body: text.into()
        }
    );

    let split = split_frontmatter(
        "---\napple-note-id: 089D915D-C76E-4F44-AB80-2190073281A3\n---\n\nBody",
        titled(),
    );
    assert!(split.frontmatter.contains("apple-note-id"));
    assert_eq!(split.body, "Body");

    assert_ne!(
        split_frontmatter("---\n\nSome prose\n\n---\n\nMore prose", SplitOptions::default()).frontmatter,
        ""
    );

    let text = "---\napple-note-id: 089D915D-C76E-4F44-AB80-2190073281A3\n---\n\n\n**Yield:** 8 servings";
    let split = split_frontmatter(text, titled());
    assert_eq!(split.body, "\n**Yield:** 8 servings");
    assert_eq!(format!("{}{}", split.frontmatter, split.body), text);

    let envelope = "---\napple-note-id: 089D915D-C76E-4F44-AB80-2190073281A3\n---\n\n";
    for body in [
        "\n\nTwo blank lines above",
        "\nOne blank line above",
        "No blank line above",
    ] {
        let split = split_frontmatter(&join_frontmatter(envelope, body), titled());
        assert_eq!(split.body, body);
        assert_eq!(split.frontmatter, envelope);
    }

    let split = split_frontmatter("---\ntags: [a]\n---\n\n\n\nMy Note\nBody", SplitOptions::default());
    assert_eq!(split.body, "My Note\nBody");
}

const ID: &str = "089D915D-C76E-4F44-AB80-2190073281A3";
const OTHER_ID: &str = "001b9e8a-c474-4311-af32-abe70026b346";

#[test]
fn read_note_id_cases() {
    assert_eq!(
        read_note_id(&format!("---\napple-note-id: {ID}\n---\n\n")).as_deref(),
        Some(ID)
    );
    assert_eq!(
        read_note_id(&format!("---\napple-note-id: \"{ID}\"\n---\n")).as_deref(),
        Some(ID)
    );
    assert_eq!(
        read_note_id(&format!("---\napple-note-id: '{ID}'\n---\n")).as_deref(),
        Some(ID)
    );
    assert_eq!(
        read_note_id(&format!("---\napple-note-id: >-\n  {ID}\n---\n")).as_deref(),
        Some(ID)
    );
    assert_eq!(
        read_note_id(&format!("---\napple-note-id: {OTHER_ID}\n---\n")).as_deref(),
        Some(OTHER_ID)
    );
    assert_eq!(
        read_note_id(&format!("---\naliases:\n  - apple-note-id: {ID}\n---\n")),
        None
    );
    assert_eq!(
        read_note_id(&format!("---\nnotes: |\n  apple-note-id: {ID}\n---\n")),
        None
    );
    assert_eq!(
        read_note_id(&format!("---\ntags: [unclosed\napple-note-id: {ID}\n---\n")),
        None
    );
    assert_eq!(read_note_id("---\napple-note-id: not-a-uuid\n---\n"), None);
    assert_eq!(read_note_id("---\napple-note-id: 12345\n---\n"), None);
    assert_eq!(read_note_id("---\napple-note-id:\n---\n"), None);
    assert_eq!(read_note_id(""), None);
    assert_eq!(read_note_id("# Just a note\n"), None);
}

#[test]
fn set_note_id_cases() {
    assert_eq!(set_note_id("", ID), format!("---\napple-note-id: {ID}\n---\n\n"));

    let result = set_note_id("---\ntags: [recipes]\n---\n\n", ID);
    assert!(result.starts_with("---\n") && result.contains("tags:"));
    assert_eq!(read_note_id(&result).as_deref(), Some(ID));

    let result = set_note_id("---\n# my notes\ntags: [recipes]\nstatus: done\n---\n\n", ID);
    assert!(result.contains("# my notes"));
    assert!(result.find("tags:") < result.find("status:"));

    let frontmatter = format!("---\ntags: [ recipes ]   # spacing we must not touch\napple-note-id: {ID}\n---\n\n");
    assert_eq!(set_note_id(&frontmatter, ID), frontmatter);

    let result = set_note_id(&format!("---\napple-note-id: {OTHER_ID}\n---\n\n"), ID);
    assert_eq!(read_note_id(&result).as_deref(), Some(ID));
    assert!(!result.contains(OTHER_ID));

    let broken = "---\ntags: [unclosed\n---\n\n";
    assert_eq!(set_note_id(broken, ID), broken);
    let scalar = "---\njust a bare scalar\n---\n\n";
    assert_eq!(set_note_id(scalar, ID), scalar);

    assert!(set_note_id("---\ntags: [recipes]\n---\n\n", ID).ends_with("---\n\n"));
    for start in [
        "",
        "---\ntags: [a]\n---\n\n",
        &format!("---\napple-note-id: {OTHER_ID}\nx: 1\n---\n\n"),
    ] {
        assert_eq!(read_note_id(&set_note_id(start, ID)).as_deref(), Some(ID), "{start:?}");
    }
}

#[test]
fn clear_note_id_cases() {
    let result = clear_note_id(&format!("---\ntags: [recipes]\napple-note-id: {ID}\n---\n\n"));
    assert_eq!(read_note_id(&result), None);
    assert!(result.contains("tags:"));
    assert_eq!(clear_note_id(&format!("---\napple-note-id: {ID}\n---\n\n")), "");
    let frontmatter = "---\ntags: [recipes]\n---\n\n";
    assert_eq!(clear_note_id(frontmatter), frontmatter);
    assert_eq!(clear_note_id(""), "");
}

#[test]
fn note_id_shape() {
    assert!(is_note_id(ID));
    assert!(is_note_id(OTHER_ID));
    assert!(!is_note_id(""));
    assert!(!is_note_id("AccountData"));
    assert!(!is_note_id(&format!("{ID}-extra")));
    for bad in ["REC1", "", "not-a-uuid", "AccountData"] {
        assert_eq!(set_note_id("", bad), "");
        assert_eq!(set_note_id("---\ntags: [a]\n---\n\n", bad), "---\ntags: [a]\n---\n\n");
    }
}

fn long_title() -> String {
    "A title far too long to be a file name, ".repeat(3)
}

#[test]
fn note_title_cases() {
    assert_eq!(
        read_note_title("---\napple-note-title: Some title\n---\n\n").as_deref(),
        Some("Some title")
    );
    assert_eq!(read_note_title("---\napple-note-title: \"\"\n---\n\n"), None);
    assert_eq!(read_note_title("---\napple-note-title: [a, b]\n---\n\n"), None);
    assert_eq!(read_note_title(&format!("---\napple-note-id: {ID}\n---\n\n")), None);
    assert_eq!(read_note_title(""), None);
    assert_eq!(read_note_title("---\n: : :\n---\n\n"), None);

    let long = long_title();
    for title in [
        ".hidden",
        "CON",
        "Trailing space ",
        "He said \"no\": really",
        long.as_str(),
    ] {
        assert_eq!(
            read_note_title(&set_note_title("", title)).as_deref(),
            Some(title),
            "lost {title:?}"
        );
        assert_eq!(
            read_note_title(&set_note_title(&format!("---\napple-note-id: {ID}\n---\n\n"), title)).as_deref(),
            Some(title)
        );
    }

    let envelope = set_note_title(&format!("---\napple-note-id: {ID}\n---\n\n"), &long);
    assert_eq!(set_note_title(&envelope, &long), envelope);

    let cleared = clear_note_title(&format!(
        "---\ntags: [a]\napple-note-id: {ID}\napple-note-title: {long}\n---\n\n"
    ));
    assert_eq!(read_note_title(&cleared), None);
    assert_eq!(read_note_id(&cleared).as_deref(), Some(ID));
    assert!(cleared.contains("tags:"));

    let envelope = format!("---\napple-note-id: {ID}\n---\n\n");
    assert_eq!(clear_note_title(&envelope), envelope);
}

#[test]
fn compose_note_file_cases() {
    let long = long_title();
    let with_title = compose_note_file("", "Body", ID, Some(&long));
    assert_eq!(read_note_title(&with_title).as_deref(), Some(long.as_str()));
    assert_eq!(read_note_id(&with_title).as_deref(), Some(ID));

    let split = split_frontmatter(&with_title, SplitOptions::default());
    let without = compose_note_file(&split.frontmatter, "Body", ID, None);
    assert_eq!(read_note_title(&without), None);
    assert_eq!(read_note_id(&without).as_deref(), Some(ID));

    let mine = "---\ntags: [recipes]\n---\n\n";
    assert!(compose_note_file(mine, "Body", ID, Some(&long)).contains("tags:"));
    assert!(compose_note_file(mine, "Body", ID, None).contains("tags:"));

    // Every vault note file: exactly this shape.
    assert_eq!(
        compose_note_file("", "# T", ID, None),
        format!("---\napple-note-id: {ID}\n---\n\n# T")
    );
}
