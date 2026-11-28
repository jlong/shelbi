# Test fixtures

- `merge/` and `reserialize/`: copied from ContextStore
  (32pixelsCo/ContextStore@8c2fdc180c5670fae1165c5d8c939c247e5ddc53,
  `ContextStoreCore/fixtures/`), MIT in Shelbi. Read by
  `merge_fixture_corpus.rs` and `merge_reserialize_snapshot.rs`. Every directory
  under `merge/` is a case.
- `planning-space/`: a subset of this repo's `docs/planning` branch (at
  82605ebc), the ContextStore space Shelbi's plans live in, copied byte for byte.
  Read by `space_round_trip.rs`, which only ever writes to a temp copy.
