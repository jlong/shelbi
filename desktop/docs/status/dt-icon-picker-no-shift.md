# dt-icon-picker-no-shift: App icon tiles that don't move when picked

## The change

Picking an icon in Settings ▸ General ▸ App icon made the tiles jump. The
picked tile's label turned loud and medium, which widened it; each tile was
sized by the wider of its well and its label, so the row reflowed.

The tiles now show no label:

```
App icon
┌────────┐  ┌────────┐
│  [CG]  │  │ [Retro]│      picking moves the ring only
└────────┘  └────────┘
Shown in the Dock while Shelbi runs and in About. ...
```

## How it works

- `elements::controls::icon_choice` is the well alone. Its `name` (the
  `AppIcon::label()`) moves into the tile's tooltip (`style::tooltip`, as
  the desktop's other icon-only controls carry their names), so it isn't
  lost.
- The picked ring (2px `check_fill`) and the `check_glow` around it were
  already absolute overlays over the well; they stay that way, so a picked
  tile and an unpicked one have identical bounds. The row's 3px padding and
  16px gap keep the glow's room.

## Tests

- `the_app_icon_tiles_pick_the_icon_live_and_save_it` asserts both tiles'
  bounds are equal before and after picking Retro and then Command Grille.
