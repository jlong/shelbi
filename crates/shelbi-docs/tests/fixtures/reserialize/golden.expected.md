# Reserialize Snapshot

This fixture covers the markdown constructs the merge pipeline expects to round-trip through `comrak::parse_document` and `comrak::format_commonmark` with the workspace-shared options. The snapshot lives alongside it as `golden.expected.md`.

## Headings

### Subsection level three

Body text under a level-3 heading.

## Inline formatting

A paragraph with **bold**, *italic*, `inline code`, and a [link](https://example.com).

## Lists

- bullet one
- bullet two with **bold** and `code`
- bullet three

<!-- end list -->

1. ordered one
2. ordered two
3. ordered three

<!-- end list -->

- [ ] task open
- [x] task done

## Blockquote

> Quoted line with *emphasis*.

## Code fence

```rust
fn main() {
    println!("hello");
}
```

## Final paragraph

A short trailing paragraph to anchor the document's tail through reserialization.
