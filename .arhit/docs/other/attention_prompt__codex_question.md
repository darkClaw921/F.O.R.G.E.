# attention_prompt::codex_question

Detects the native Codex structured request_user_input overlay: a valid question progress header, nonempty question/answer region and final answer/all-answer submission hints plus interrupt, notes or question-navigation hints. Handles choice, freeform, typed-answer, notes, custom submit/interrupt keys and wrapped footers. Normal shortcut/composer rows after old questions reject them. Does not infer that arbitrary prose ending in a question mark requires input.
