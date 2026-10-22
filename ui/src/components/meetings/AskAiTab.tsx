export type AskAiTabProps = {
  meetingId: string;
  /** The meeting is still recording, so edits to its notes are not offered. */
  recording: boolean;
};

/**
 * Placeholder body for the Ask AI tab.
 *
 * The chat arrives in a later change and replaces this component wholesale.
 * Until then the tab says only what it is for, and offers nothing to click.
 */
export default function AskAiTab({ meetingId, recording }: AskAiTabProps) {
  return (
    <p
      className="kea-muted"
      data-meeting-id={meetingId}
      data-recording={recording ? "true" : undefined}
      style={{ margin: 0 }}
    >
      Ask questions about this meeting.
    </p>
  );
}
