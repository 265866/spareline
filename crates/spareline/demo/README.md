# Demo and screenshots

Read this to run spareline with invented phones and texts, or to recapture the screenshots in
`docs/screenshots/`. You need cargo, Bun, Python 3, and uv for the capture.

```sh
bash crates/spareline/demo/run.sh             # serve on http://127.0.0.1:18790 until Ctrl-C
bash crates/spareline/demo/run.sh --capture   # rewrite docs/screenshots/*.png, then stop
```

`run.sh` builds spareline and the web app, then starts both on a new temporary directory, which it
deletes when it stops. It prints the demo user's password. `--no-build` skips the builds, and
`--port` and `--adb-port` move the ports from 18790 and 15090.

- **The phones are invented.** `adb.py` is a stand-in adb server on loopback. It serves three
  phones in the output formats real phones print: two online with three SIMs between them, and
  one waiting for "Allow USB debugging". spareline reads them and ingests their texts through its
  usual code, so nothing in the demo writes to the database directly. Nothing leaves the machine:
  `adb.py` reaches no phone and no real adb server, and spareline talks only to `adb.py`.
- **Every value is invented.** Phone numbers come from 555-0100 to 555-0199, which the North
  American Numbering Plan keeps for fiction. The serials, IMEIs, ICCIDs, carriers, senders and
  texts are made up, as are the phones' make and model, and the SIMs report the test network code
  001-01.
- **Same data every run.** Every time `adb.py` prints is an offset from when it started. It hands
  the texts over one at a time, oldest first, so spareline ingests them in the order they were
  sent. The demo takes about a minute to fill.
- **Privacy check.** Before each screenshot, `capture.py` refuses the page if it shows any of
  these:
  - a phone number outside the fiction range, or a run of seven or more digits that isn't one of
    the demo's IMEIs or ICCIDs;
  - an email address outside `example.com` and `example.org`;
  - an IPv4 address other than `127.0.0.1` and the documentation range `192.0.2.x`;
  - a tailnet name;
  - a serial-like token that isn't one of the demo's;
  - your user name, host name or home directory.

  Pass `--deny a,b` to `run.sh` to refuse more strings, such as a host or account name. Run
  `uv run --script crates/spareline/demo/capture.py --self-test` to test the rules.
- **Two sizes, dark theme.** `--capture` writes each page in the dark theme at 1440 by 900 pixels
  as `<page>.png`, and at a phone size, 390 by 844 at twice the density, as `<page>-mobile.png`. The Screens page is left out,
  since its video needs a real phone.
- **Read the images** before you commit them.
