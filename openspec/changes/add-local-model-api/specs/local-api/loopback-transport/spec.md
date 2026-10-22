# Spec Delta

## Purpose

Defines how clients reach the local API: the default Unix socket, the opt-in localhost
TCP port that OpenAI-compatible clients need, and the authentication rules both
transports share.

## ADDED Requirements

### Requirement: The localhost port is opt-in and loopback-only
The system SHALL offer a setting, separate from enabling the local API and off by
default, that also serves the local API on a TCP port bound to `127.0.0.1` only. The
port SHALL default to 8765 and be configurable. The system MUST NOT bind any
non-loopback address. Turning the setting on SHALL show, in the same view, what holding
the API token allows: reading the selection, typing into the focused app, opening the
microphone, and spending LLM credits.

#### Scenario: Default posture
- **WHEN** the local API is enabled and the TCP setting has never been touched
- **THEN** only the Unix socket is listening

#### Scenario: Port enabled
- **WHEN** the user enables the TCP setting with port 8765
- **THEN** the API answers on `http://127.0.0.1:8765` as well as on the socket, and nothing listens on any other interface

#### Scenario: Port already in use
- **WHEN** the chosen port is taken by another process
- **THEN** the setting reports the conflict, the socket keeps working, and nothing else is bound in its place

#### Scenario: Turning it off
- **WHEN** the user disables the TCP setting
- **THEN** the port stops accepting connections immediately, and in-flight requests on it are allowed to finish

### Requirement: The same authentication applies on every transport
Every request on either transport MUST:
- carry the API token, as `Authorization: Bearer <token>` or as `x-kea-token: <token>`;
- carry no `Origin` header;
- carry a `Host` header naming `localhost`, `127.0.0.1` or `[::1]`, with or without a port.

The system MUST NOT send any CORS `Access-Control-Allow-*` header. Token comparison
MUST be constant-time. Every rejection SHALL be `401` and SHALL NOT say which check
failed.

#### Scenario: OpenAI SDK key
- **WHEN** an OpenAI SDK is configured with `base_url=http://127.0.0.1:8765/v1` and the KEA token as its API key
- **THEN** its requests authenticate

#### Scenario: Browser page
- **WHEN** a web page in a browser makes a request to `http://127.0.0.1:8765/v1/models`
- **THEN** the request is refused, because it carries an `Origin` header or cannot pass the CORS preflight

#### Scenario: DNS rebinding
- **WHEN** a request reaches the port with `Host: attacker.example`
- **THEN** it is refused with `401`

### Requirement: The port brings the API to platforms without Unix sockets
On a platform where the Unix-socket transport is unavailable, the local API SHALL still
be usable through the TCP setting, and the settings view SHALL say that the port is the
only transport there.

#### Scenario: Windows
- **WHEN** a Windows user enables the local API and the TCP setting
- **THEN** the API, including the model routes, answers on `127.0.0.1:<port>`

#### Scenario: Windows without the port
- **WHEN** a Windows user enables the local API but not the TCP setting
- **THEN** the settings view says the API needs the localhost port on this platform
