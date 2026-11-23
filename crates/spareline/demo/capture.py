# /// script
# requires-python = ">=3.11"
# dependencies = ["playwright>=1.49"]
# ///
"""Captures the demo's pages for docs/screenshots/, each at a desktop and a phone size, and refuses
a page that shows a value outside the demo's invented set. run.sh --capture starts the demo and
runs this; to run it against a demo that is already up:

    uv run --script capture.py --url http://127.0.0.1:18790 --password-file FILE --out DIR
    uv run --script capture.py --self-test

--deny a,b refuses more strings, such as a host or account name. The current user name, host
name and home directory are always refused.
"""

import argparse
import getpass
import os
import re
import socket
import subprocess
import sys
from pathlib import Path

DESKTOP = {"width": 1440, "height": 900}
PHONE = {"width": 390, "height": 844}

# name, path, what the page is waiting for, what to do before the capture. The Screens page is
# left out: its video comes from scrcpy on a real phone, which the demo has none of.
SHOTS = [
    ("phones", "/", "text=DEMO0A11CE01", None),
    ("numbers", "/numbers", "text=Bike sale listing", None),
    ("number", "/numbers/+15125550142", "text=see you at 10", None),
    ("messages", "/messages", "text=All messages", None),
    ("messages-search", "/messages?q=Fernbank", "text=verification code", None),
    ("settings", "/settings", "text=API keys", None),
    ("sign-in", "/", "input[type=password]", "sign-out"),
]

# The demo's values, from adb.py and run.sh.
DEMO_SERIALS = {"DEMO0A11CE01", "DEMO0B22CE02", "DEMO0C33CE03"}
ALLOWED_DIGITS = [
    re.compile(r"1?\d{3}5550[01]\d{2}"),  # 555-0100 to 555-0199: kept for fiction
    re.compile(r"35000000000\d{4}"),  # the demo's IMEIs
    re.compile(r"8900100000000000\d{3}"),  # the demo's ICCIDs
]
DATE_OR_TIME = re.compile(r"\b\d{4}-\d{2}-\d{2}(?:[T ]\d{2}:\d{2}(?::\d{2}(?:\.\d+)?)?Z?)?\b|\b\d{1,2}:\d{2}(?::\d{2})?\b")
NUMBER_SHAPES = [
    re.compile(r"(?<![\d+])(?:\+?1[ .-]?)?\(?\d{3}\)?[ .-]?\d{3}[ .-]?\d{4}(?!\d)"),
    re.compile(r"\+\d{1,3}(?:[ .-]?\d){6,14}(?!\d)"),
    re.compile(r"(?<!\d)\d{7,}(?!\d)"),
]
EMAIL = re.compile(r"[\w.+-]+@([\w-]+\.)+\w+")
IPV4 = re.compile(r"\b\d{1,3}(?:\.\d{1,3}){3}\b")
SERIAL_LIKE = re.compile(r"\b(?=[A-Z0-9]*\d[A-Z0-9]*\d[A-Z0-9]*\d)(?=[A-Z0-9]*[A-Z][A-Z0-9]*[A-Z])[A-Z0-9]{10,16}\b")


def problems(text, deny):
    """What `text` shows that isn't the demo's, as short descriptions."""
    found = []
    scrubbed = DATE_OR_TIME.sub(" ", text)
    for shape in NUMBER_SHAPES:
        for match in shape.finditer(scrubbed):
            digits = re.sub(r"\D", "", match.group())
            if not any(rule.fullmatch(digits) for rule in ALLOWED_DIGITS):
                found.append(f"a number outside the demo set: {match.group().strip()}")
    for match in EMAIL.finditer(text):
        if not match.group().lower().endswith(("@example.com", "@example.org")):
            found.append(f"an email address: {match.group()}")
    for match in IPV4.finditer(text):
        if match.group() != "127.0.0.1" and not match.group().startswith("192.0.2."):
            found.append(f"an IPv4 address: {match.group()}")
    if ".ts.net" in text:
        found.append("a tailnet name")
    for match in SERIAL_LIKE.finditer(text):
        if match.group() not in DEMO_SERIALS:
            found.append(f"a serial-like value: {match.group()}")
    for word in deny:
        if word and word.lower() in text.lower():
            found.append(f"a refused string: {word}")
    return found


def self_test():
    deny = ["examplehost"]
    clean = (
        "+1 512 555 0142 (512) 555-0177 +17375550118 DEMO0A11CE01 350000000000106 "
        "8900100000000000011 2026-11-23T09:16:40.830Z 3:40 PM code 418207 order #20417 $180 "
        "RW-5 someone@example.com 127.0.0.1"
    )
    assert problems(clean, deny) == [], problems(clean, deny)
    for dirty in [
        "+1 415 867 5309",
        "(512) 555-0200",
        "IMEI 490154203237518",
        "8901260000000000000",
        "me@gmail.com",
        "10.1.2.3",
        "host.tailnet-1234.ts.net",
        "R58M12ABCDE",
        "EXAMPLEHOST",
    ]:
        assert problems(dirty, deny), f"not refused: {dirty}"
    print("privacy rules: ok")


def capture(args):
    from playwright.sync_api import sync_playwright

    deny = [getpass.getuser(), socket.gethostname(), os.path.expanduser("~")]
    deny += [word for word in (args.deny or "").split(",") if word]
    password = Path(args.password_file).read_text().strip()
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    subprocess.run([sys.executable, "-m", "playwright", "install", "chromium"], check=True)
    refused = []
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch()
        for suffix, viewport, scale in (("", DESKTOP, 1), ("-mobile", PHONE, 2)):
            context = browser.new_context(
                viewport=viewport, device_scale_factor=scale, color_scheme="dark",
                timezone_id="America/Chicago", locale="en-US",
            )
            page = context.new_page()
            response = page.request.post(
                f"{args.url}/api/v1/auth/login", data={"name": "demo", "password": password}
            )
            assert response.ok, f"sign-in failed: {response.status}"
            for name, path, ready, before in SHOTS:
                if before == "sign-out":
                    page.request.post(f"{args.url}/api/v1/auth/logout")
                    context.clear_cookies()
                page.goto(f"{args.url}{path}")
                page.wait_for_selector(ready, timeout=15_000)
                page.wait_for_load_state("networkidle")
                page.wait_for_timeout(500)
                text = page.evaluate(
                    "() => document.body.innerText + '\\n' + "
                    "[...document.querySelectorAll('input,textarea')].map(e => e.value).join('\\n')"
                )
                found = problems(text, deny)
                file = out / f"{name}{suffix}.png"
                if found:
                    refused.append((file.name, found))
                    continue
                page.screenshot(path=str(file))
                print(f"wrote {file}")
            context.close()
        browser.close()
    for name, found in refused:
        print(f"REFUSED {name}: " + "; ".join(found), file=sys.stderr)
    return 1 if refused else 0


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--url")
    parser.add_argument("--password-file")
    parser.add_argument("--out")
    parser.add_argument("--deny")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    self_test()
    if args.self_test:
        return 0
    return capture(args)


if __name__ == "__main__":
    sys.exit(main())
