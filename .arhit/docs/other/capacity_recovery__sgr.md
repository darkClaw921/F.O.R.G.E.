# capacity_recovery::sgr

Updates dim, background and red-foreground style from semicolon SGR parameters. Reset0 clears all attributes;22 clears dim. Extended38/48;2;r;g;b and38/48;5;n operands are consumed as one color instruction so truecolor selector2 never becomes dim. Unknown malformed color/control data is rejected. Native color metadata is used only for detecting an actual error and an empty composer.
