# Spec Delta

## Purpose

Gives shell users and scripts one-line access to KEA's models through the `kea`
command, with text and audio passed through files, standard input and standard output
so commands compose in pipelines.

## ADDED Requirements

### Requirement: Speech-to-text from the command line
`kea stt` SHALL take an audio or video file path, or `-` for standard input, and print
the transcript to standard output. It SHALL accept `--format text|json|srt|vtt|verbose_json`
(default `text`), `--language`, `--model` and `--speakers`.

#### Scenario: Transcribe a file
- **WHEN** the user runs `kea stt call.m4a`
- **THEN** the transcript is printed as plain text and the exit status is 0

#### Scenario: Pipe audio in
- **WHEN** the user runs `ffmpeg -i in.mp4 -f wav - | kea stt -`
- **THEN** the transcript of the piped audio is printed

#### Scenario: Subtitles with speakers
- **WHEN** the user runs `kea stt talk.wav --format srt --speakers`
- **THEN** an SRT document is printed with each cue prefixed by its speaker label

### Requirement: Text-to-speech from the command line
`kea tts` SHALL take text as an argument, or `-` for standard input, and write WAV
audio to `--out FILE`. When `--out` is not given and standard output is not a terminal,
it SHALL write to standard output. It SHALL accept `--voice`, `--speed` and `--model`.
It MUST NOT write binary audio to a terminal; in that case it SHALL exit non-zero and
ask for `--out`.

#### Scenario: Write a file
- **WHEN** the user runs `kea tts "Build finished" --out done.wav`
- **THEN** `done.wav` contains the synthesized speech

#### Scenario: Pipe to a player
- **WHEN** the user runs `kea tts "Hello" | afplay -`
- **THEN** the WAV goes to standard output

#### Scenario: Terminal guard
- **WHEN** the user runs `kea tts "Hello"` in an interactive terminal without `--out`
- **THEN** nothing binary is printed, the command exits non-zero, and the message suggests `--out`

### Requirement: LLM prompts from the command line
`kea llm` SHALL take a prompt as an argument, or `-` for standard input, send it to the
LLM bound for API use, and print the reply. It SHALL accept `--system TEXT` and
`--model`. When both an argument and piped input are present, the piped text SHALL be
appended to the prompt, so `git diff | kea llm "Summarise this diff"` works.

#### Scenario: Prompt with piped context
- **WHEN** the user runs `git diff | kea llm "Write a commit message for this"`
- **THEN** the reply is printed, generated from the instruction followed by the diff

### Requirement: Listing models from the command line
`kea models` SHALL print one line per model the API accepts, with its capability and a
marker on the current default for each capability. `--json` SHALL print the raw list.

#### Scenario: See what is available
- **WHEN** the user runs `kea models`
- **THEN** each installed or bound model is shown with `stt`, `tts` or `llm`, and the defaults are marked

### Requirement: Commands fail clearly
Every model command SHALL print the API's error message to standard error and exit
non-zero on any failure. It SHALL distinguish "the local API is not running", "the
token cannot be read" and an error returned by the API itself. The commands SHALL use
the Unix socket by default, and the TCP port when `--api-url` or `KEA_API_URL` is set.

#### Scenario: API off
- **WHEN** the local API is disabled and the user runs `kea models`
- **THEN** the command says to turn on the local API in KEA's General settings and exits with the existing "unavailable" status

#### Scenario: Model not bound
- **WHEN** no text-to-speech model is bound and the user runs `kea tts "hi" --out a.wav`
- **THEN** the API's `409` message about the missing binding is printed and the exit status is non-zero
