# The `mail` reagent

`mail` is the one reagent in this version. It works on a fixture mailbox of plain files, which makes it safe to experiment with and easy to inspect.

## Mailbox layout

Set `mail_root` in the [config](config.md). Under it, each folder is a directory and each message is a file:

```text
mailbox/
  Inbox/
    2026-11-01.eml
    2026-11-02.eml
  Receipts/
  Newsletters/
  Action/
```

- Files ending in `.eml` or `.txt` are messages. The file name without its extension is the message **id**.
- "Newest" means the largest id when sorted as text. Date-style names such as `2026-11-14` sort correctly.
- `From`, `Subject` and `Date` are read from header lines before the first blank line.
- A folder is created when a message is first moved into it.

## Tools

| Tool | Effect | Arguments | Returns |
| --- | --- | --- | --- |
| `search` | read | `folder` (required), `newest` (default 50) | `[{id, from, subject, date}]`, newest first |
| `read` | read | `message` (required), `max_chars` (default 6000) | `{id, folder, from, subject, text, truncated}` |
| `move` | idempotent write | `message`, `folder` (both required) | `{id, from, to, moved}`; or `already_there: true` |
| `locate` | read | `message` (required) | `{id, folder, exists}` |

`summonr satchel` prints this list.

## Rules it enforces

These checks live in the reagent, not in any prompt.

1. **Selection.** `search` adds every id it returns to the summon's *selection*. `read`, `move` and `locate` refuse any message not in the selection, whatever id they were given. A model cannot invent its way to another message.
2. **Grants.** `search` needs a grant on `folder`. `read` needs a grant on the folder the message is in now. `move` needs a grant `on` the source folder `into` the destination. `locate` needs a `locate` grant.
3. **Plain names.** Folder names and ids that are empty, contain `/` or `\`, or start with `.` are refused, so nothing can escape the mailbox.
4. **Idempotence.** Moving a message to the folder it is already in changes nothing and returns `already_there: true`. Repeating a completed move is harmless.

Refusals are recorded as `not authorized: ...` and fail the step.

## Known limitation: duplicate ids

Selection is tracked by id alone, and `locate` finds a message by searching every folder for that file name. If two folders contain a message with the same id, a `read`, `move` or `locate` after searching one folder may resolve to the copy in the other. With narrow grants this fails as "not authorized". With broad grants it could act on the wrong message. Keep ids unique across folders until selection records the folder too.
