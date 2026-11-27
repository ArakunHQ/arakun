# arakun-pilot, the pull request agent

With a pull request agent on a server of your own
(arakun-pilot), Ctrl+K "Connect arakun-pilot…" (also in the GitHub tab's `…` menu,
and under an empty queue) asks for the Worker's address, its token and, to send it
tasks, its admin token; it checks the token against the Worker before it saves them
(the admin token is saved unchecked, since only sending a task tries it), and
later "arakun-pilot settings…" changes them or disconnects. Under that row the
`…` menu says how pilot is doing ("Connected to … · read 2 min ago", or what is
wrong), and when pilot can't be read, or its settings file can't be used, the
queue says why with a "Settings…" link to the form. From a terminal,
`arakun github pilot connect <url> --token -` (the token on stdin) does the same;
`arakun github pilot status` says whether they still work and
`arakun github pilot disconnect` removes them. A token never goes in the command line,
where `ps` and shell history would show it: `--token -` reads it from stdin, else
`ARAKUN_PILOT_TOKEN`, else the token already saved for the same address. The admin
token comes the same way, from `--admin-token -` or `ARAKUN_PILOT_ADMIN_TOKEN` (only
one of the two can come from stdin); `--no-admin-token` removes the saved one, and
`--no-check` saves without asking the Worker first. An empty stdin is an error. The tokens
go in the system keyring and the address in `~/.config/arakun-pilot/client.json`
(`$XDG_CONFIG_HOME` when set to an absolute path; `{"url": "…"}`, readable only by you), and
Arakun reads `GET <url>/board` and `GET <url>/items` every minute. A `token` or
`admin_token` written in that file by hand moves to the keyring the first time Arakun reads
it. A test Arakun with `ARAKUN_STATE` set, as `arakun dev` starts one, reads
`arakun-pilot/client.json` in the folder of that state file instead, and keeps its own tokens.
The `url` must be `https://`; plain `http://` works only for `localhost`, `127.0.0.1`
and `[::1]`, so the tokens never cross a network unencrypted. The board fills the
queue; the rows pilot doesn't follow fold under "More on GitHub". When pilot
asks you something, the sheet shows the question with one button per answer and
"Other answer…" for your own words ("Answer…" when there are no ready answers;
`POST <url>/items/<id>/answer`), and the notice in
the island opens that sheet over whatever window is in front (Esc gives it back).
Without a board, the notice opens the pull request's sheet, which doesn't show the
question: answer it on pilot's card in the queue. On your own pull requests, "Ask pilot" in the sheet sends what you
typed as a task (`POST <url>/tasks`, which needs the admin token).
While the Worker has no board, its cards show at the top of the queue under "Your
agent". With a board, a card about a pull request the board follows shows under
"From pilot" in that pull request's sheet, and the rest in a "From pilot" group at the
top of the queue. "Looks good" approves the commit pilot read, and both it and
request changes go through the same second click. What you saw and dealt with is
kept in `~/.local/share/arakun/pilot.json` (`%LOCALAPPDATA%\arakun\pilot.json` on
Windows; next to the state file when `ARAKUN_STATE` is set). Ctrl+K "Send to the cloud agent…" (or type
`cloud: <task>`) hands it new work, on the open pull request of yours picked in the tab
or on a new branch (`POST <url>/tasks`); that needs the admin token
too. On that card `Tab` switches between your pull request and a new branch (see
[Keyboard shortcuts](shortcuts.md)).
