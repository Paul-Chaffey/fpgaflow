# images

Flash images read from boards (Tools › Read flash…, or `fpgaflow flash read`)
are saved here by default. They are kept out of git: a board's flash can hold
other people's bitstreams (the Celoxica RC10's holds Celoxica's demos).

- Spartan-3AN Starter Kit: the XCF04S Platform Flash as raw bytes (`.bin`).
- Celoxica RC10: every file in its flash, in one `.tar` with an `index.txt`.
