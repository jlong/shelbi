# Linear fixtures

Canned GraphQL responses for `shelbi_state::linear_client::FakeLinear`, one
file per operation name (`Viewer` -> `viewer.json`, `TeamStates` ->
`team_states.json`, ...). The fake embeds them with `include_str!`, so a
consumer crate's tests get them without knowing this path.

The workspace they describe: viewer `ada` (`user-ada`); teams `ENG`
(`team-eng`, Engineering) and `DES` (`team-des`, Design); seven ENG states
across all six types; projects `Shelbi` and `Mobile`; labels `Bug`, `shelbi`,
`Improvement` and the group label `Feature`; members ada, grace, two `sam`s
(for ambiguity) and one inactive user. Every list is a single page; tests that
need pagination queue their own pages.
