/**
 * The "how to call it" blurb under the Local API section.
 *
 * Shows the base URL the API answers on, a short OpenAI SDK example, and what
 * the OpenAI-compatible routes accept. The token is only ever a placeholder
 * here: this text is often screen-shared or pasted into notes, and the real
 * token stays behind Reveal.
 */

/** The key in the examples. A placeholder, never the token itself. */
const TOKEN_PLACEHOLDER = "<your KEA access token>";

type Props = {
  /** The localhost port the API listens on, or would listen on. */
  port: number;
  /** Whether the localhost port is switched on. */
  portOn: boolean;
  /** The unix socket path, or null where there is no socket on this platform. */
  socketPath: string | null;
};

export default function ApiUsage({ port, portOn, socketPath }: Props) {
  const baseUrl = `http://127.0.0.1:${port}/v1`;

  return (
    <div style={{ marginTop: 24 }}>
      <h3 style={{ margin: "0 0 8px" }}>Using the API</h3>

      {portOn ? (
        <p className="kea-muted" style={{ margin: "0 0 8px" }}>
          Base URL: <code>{baseUrl}</code>
        </p>
      ) : socketPath ? (
        <>
          <p className="kea-muted" style={{ margin: "0 0 8px" }}>
            The unix socket is the default transport. Most SDKs can only reach a URL, so turn on
            the localhost port above to use them. From the shell, use curl:
          </p>
          <pre style={{ overflowX: "auto" }}>
            <code>{`curl --unix-socket ${socketPath} \\
  -H "Authorization: Bearer ${TOKEN_PLACEHOLDER}" \\
  http://localhost/v1/models`}</code>
          </pre>
        </>
      ) : (
        <p className="kea-muted" style={{ margin: "0 0 8px" }}>
          Turn on the localhost port above to reach the API from this platform.
        </p>
      )}

      <p className="kea-muted" style={{ margin: "0 0 8px" }}>
        With the OpenAI Python SDK, point <code>base_url</code> at the port and use the access
        token as the key:
      </p>
      <pre style={{ overflowX: "auto" }}>
        <code>{`from openai import OpenAI

client = OpenAI(
    base_url="${baseUrl}",
    api_key="${TOKEN_PLACEHOLDER}",
)
audio = client.audio.transcriptions.create(model="whisper-1", file=open("note.m4a", "rb"))`}</code>
      </pre>

      <ul className="kea-muted" style={{ margin: "0 0 8px", paddingLeft: 20 }}>
        <li>
          Transcription (<code>/v1/audio/transcriptions</code>): <code>json</code>,{" "}
          <code>text</code>, <code>verbose_json</code>, <code>srt</code> and <code>vtt</code>.
        </li>
        <li>
          Speech (<code>/v1/audio/speech</code>): <code>wav</code> or <code>pcm</code>.
        </li>
        <li>
          Chat (<code>/v1/chat/completions</code>) and <code>/v1/models</code>.
        </li>
        <li>
          <code>temperature</code> and <code>max_tokens</code> are ignored. <code>stream: true</code>{" "}
          returns the whole reply as a single chunk.
        </li>
      </ul>
    </div>
  );
}
