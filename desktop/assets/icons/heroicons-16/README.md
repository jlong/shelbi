# Heroicons at 16pt

The issue detail's close mark (`Icon / x-mark` in the Figma Issue detail
frame, 2172:914) is the Heroicons outline drawing at 16pt with a 1.5pt
stroke. `x-mark.svg` is the `24/outline` drawing from `../heroicons/` with
only `stroke-width` changed, from 1.5 to 2.25 (1.5pt once the 24 grid is
drawn at 16). A test checks it against its 20pt original.
