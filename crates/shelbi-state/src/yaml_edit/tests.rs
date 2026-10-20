use super::*;

fn v(yaml: &str) -> Value {
    parse(yaml).unwrap()
}

/// Patch `text` to `target` and check the result means `target`.
fn patched(text: &str, target: &str) -> Patched {
    let target = v(target);
    let out = patch(text, &target).unwrap();
    assert_eq!(v(&out.text), target, "patched text:\n{}", out.text);
    out
}

/// A hand-written workflow in the shapes users write: a header, blank
/// lines between blocks, flow and block transitions, comments above and
/// beside entries.
const HAND: &str = "\
## Header comment.
## Second line.

name: app
description: Rust app track.

statuses:
- id: backlog
  owner: user
  agent: orchestrator
- id: in-progress   # the developer's column
  owner: agent
  agent: developer
- id: done
  owner: user

transitions:
  # Disk cap: drop target/ on the way out.
  - from: in-progress
    to: review
    actions: [push_branch, open_pr]
    run:
      - 'rm -rf target; true'
  - { from: review,      to: done,     actions: [merge, delete_branch] }

zen:
  checks:
    local: []  # CI runs these

## Trailing commented example.
#initial_status: backlog
";

#[test]
fn an_unchanged_tree_keeps_the_text_byte_for_byte() {
    let out = patch(HAND, &v(HAND)).unwrap();
    assert_eq!(out.text, HAND);
    assert!(out.dropped_comments.is_empty());
}

#[test]
fn a_scalar_edit_touches_one_line() {
    let target = HAND.replace(
        "description: Rust app track.",
        "description: Something else",
    );
    let out = patched(HAND, &target);
    assert_eq!(out.text, target);
    assert!(out.dropped_comments.is_empty());
}

#[test]
fn an_edit_inside_a_list_item_keeps_its_neighbours_and_comments() {
    let target = HAND.replace(
        "  owner: agent\n  agent: developer",
        "  owner: user\n  agent: developer",
    );
    let out = patched(HAND, &target);
    assert_eq!(out.text, target);
    // A key added to the item goes at its end, in the item's indentation.
    let target = HAND.replace(
        "- id: done\n  owner: user\n",
        "- id: done\n  owner: user\n  tags:\n  - review\n",
    );
    assert_eq!(patched(HAND, &target).text, target);
}

#[test]
fn a_flow_list_stays_flow() {
    let target = HAND.replace(
        "actions: [push_branch, open_pr]",
        "actions: [push_branch, open_pr, merge]",
    );
    assert_eq!(patched(HAND, &target).text, target);
    let target = HAND.replace(
        "  - { from: review,      to: done,     actions: [merge, delete_branch] }",
        "  - { from: review, to: done, actions: [merge] }",
    );
    assert_eq!(patched(HAND, &target).text, target);
}

#[test]
fn a_new_item_follows_its_flow_siblings() {
    let target = HAND.replace(
        "delete_branch] }\n",
        "delete_branch] }\n  - { from: review, to: canceled, actions: [close_pr] }\n",
    );
    assert_eq!(patched(HAND, &target).text, target);
}

#[test]
fn reordering_a_list_moves_each_item_with_its_comments() {
    let mut tree = v(HAND);
    let ts = tree["transitions"].as_sequence_mut().unwrap();
    ts.swap(0, 1);
    let out = patch(HAND, &tree).unwrap();
    assert_eq!(v(&out.text), tree);
    assert!(
        out.dropped_comments.is_empty(),
        "{:?}",
        out.dropped_comments
    );
    assert!(
        out.text.contains(
            "transitions:\n  - { from: review,      to: done,     actions: [merge, delete_branch] }\n  # Disk cap: drop target/ on the way out.\n  - from: in-progress\n"
        ),
        "{}",
        out.text
    );
}

#[test]
fn removing_an_item_drops_only_its_lines() {
    let target = HAND.replace(
        "- id: in-progress   # the developer's column\n  owner: agent\n  agent: developer\n",
        "",
    );
    let out = patched(HAND, &target);
    assert_eq!(out.text, target);
    assert_eq!(out.dropped_comments, vec!["# the developer's column"]);
}

#[test]
fn a_new_key_goes_after_the_last_entry_and_before_trailing_comments() {
    let mut tree = v(HAND);
    tree.as_mapping_mut()
        .unwrap()
        .insert("initial_status".into(), "backlog".into());
    let out = patch(HAND, &tree).unwrap();
    assert_eq!(v(&out.text), tree);
    assert!(
        out.text.contains("    local: []  # CI runs these\ninitial_status: backlog\n\n## Trailing commented example.\n"),
        "{}",
        out.text
    );
}

#[test]
fn a_removed_key_takes_its_block() {
    let mut tree = v(HAND);
    tree.as_mapping_mut().unwrap().shift_remove("zen");
    let out = patch(HAND, &tree).unwrap();
    assert_eq!(v(&out.text), tree);
    assert!(!out.text.contains("zen:"));
    assert!(out
        .text
        .ends_with("\n## Trailing commented example.\n#initial_status: backlog\n"));
    assert_eq!(out.dropped_comments, vec!["# CI runs these"]);
}

#[test]
fn a_block_scalar_is_replaced_whole() {
    let text = "name: a\ndescription: |\n  Line one.\n  Line two.\nstatuses: []  # none\n";
    let out = patched(text, "name: a\ndescription: \"One line.\"\nstatuses: []\n");
    assert_eq!(
        out.text,
        "name: a\ndescription: One line.\nstatuses: []  # none\n"
    );
    let out = patched(
        text,
        "name: a\ndescription: \"Two\\nlines\\n\"\nstatuses: []\n",
    );
    assert!(
        out.text
            .starts_with("name: a\ndescription: |\n  Two\n  lines\nstatuses: []  # none\n"),
        "{}",
        out.text
    );
}

#[test]
fn awkward_strings_are_quoted() {
    let text = "a: [x]\n";
    for s in [
        "true", "1", "a, b", "x: y", "#", "it's", "", " pad", "~", "null",
    ] {
        let mut tree = Mapping::new();
        tree.insert("a".into(), Value::Sequence(vec![s.into()]));
        let tree = Value::Mapping(tree);
        let out = patch(text, &tree).unwrap();
        assert_eq!(v(&out.text), tree, "{s:?}: {}", out.text);
    }
}

#[test]
fn shapes_it_doesnt_follow_still_get_the_right_tree() {
    // Flow at the top level: the text editor can't follow it, so the tree
    // is re-serialized (and the comment goes, which the result says).
    let text = "{name: a, statuses: [b]}  # flow\n";
    let out = patched(text, "name: a\nstatuses: [b, c]\n");
    assert_eq!(out.dropped_comments, vec!["# flow"]);
    // A document marker.
    patched("---\nname: a\n", "name: b\n");
    // An empty and a comment-only file.
    assert_eq!(patched("", "a: 1\n").text, "a: 1\n");
    assert_eq!(
        patched("# just this\n", "a: 1\n").text,
        "# just this\na: 1\n"
    );
}

#[test]
fn a_broken_file_is_refused() {
    assert!(matches!(
        patch("a: [unclosed\n", &v("a: 1")),
        Err(PatchError::Parse(_))
    ));
}

#[test]
fn merge_keeps_what_the_typed_edit_didnt_change() {
    let raw = v("name: a\nfuture: kept\nstatuses:\n- {id: x, owner: user, tag: review, extra: 1}\n- {id: y, owner: user}\n");
    // The typed form spells `tag` as `tags` and doesn't know `future` or
    // `extra`.
    let base =
        v("name: a\nstatuses:\n- {id: x, owner: user, tags: [review]}\n- {id: y, owner: user}\n");
    // Unchanged: raw as it was.
    assert_eq!(merge(&raw, &base, &base, &[("tags", "tag")]), raw);
    // Change y's owner and x's tags, reorder the two.
    let new = v(
        "name: a\nstatuses:\n- {id: y, owner: agent}\n- {id: x, owner: user, tags: [review, qa]}\n",
    );
    let out = merge(&raw, &base, &new, &[("tags", "tag")]);
    assert_eq!(
        out,
        v("name: a\nfuture: kept\nstatuses:\n- {id: y, owner: agent}\n- {id: x, owner: user, extra: 1, tags: [review, qa]}\n")
    );
    // A typed field gone from the edit is removed from the file.
    let new = v("statuses:\n- {id: x, owner: user}\n- {id: y, owner: user}\n");
    let out = merge(&raw, &base, &new, &[("tags", "tag")]);
    assert_eq!(
        out,
        v("future: kept\nstatuses:\n- {id: x, owner: user, extra: 1}\n- {id: y, owner: user}\n")
    );
}

#[test]
fn merge_leaves_typed_defaults_out_of_the_file() {
    // `merge_strategy` serializes even when the file omits it; an edit to
    // another git field mustn't write it.
    let raw = v("git:\n  base_branch: main\n");
    let base = v("git:\n  base_branch: main\n  merge_strategy: squash\n");
    let new = v("git:\n  base_branch: develop\n  merge_strategy: squash\n");
    assert_eq!(
        merge(&raw, &base, &new, &[]),
        v("git:\n  base_branch: develop\n")
    );
}

#[test]
fn trailing_comments_are_found_outside_quotes() {
    assert_eq!(trailing_comment("a: b  # c"), Some("# c"));
    assert_eq!(trailing_comment("a: 'b # c'"), None);
    assert_eq!(trailing_comment("- 'x#y' # z"), Some("# z"));
    assert_eq!(trailing_comment("a: john's # c"), Some("# c"));
    assert_eq!(trailing_comment("# whole line"), None);
    assert_eq!(trailing_comment("a: b#c"), None);
}
