import { forwardRef, useEffect, useRef, useState } from "react";
import { REWRITE_MODES, type CustomStyle } from "../api";
import type { RewriteSettingsController } from "../hooks/useRewriteSettings";
import Section from "./Section";
import { Row, RowGroup } from "./SettingsRow";

type Props = {
  rewrite: RewriteSettingsController;
  /**
   * Bumped by the Style menu's "+ New style…" entry. The form lives down here,
   * so the menu cannot open it directly; a changing number is the smallest
   * thing that says "do it again" without the parent holding the form's state.
   */
  newStyleRequest: number;
};

/**
 * Why `name` cannot be used, or null when it can.
 *
 * The built-in labels count as taken: a custom "improve" next to the built-in
 * Improve would be two identical entries in the picker, and the user would
 * have no way to tell them apart.
 */
export function nameProblem(
  name: string,
  styles: CustomStyle[],
  editingId?: string,
): string | null {
  const wanted = name.trim().toLowerCase();
  if (!wanted) return "Give the style a name.";
  const taken =
    REWRITE_MODES.some((m) => m.label.toLowerCase() === wanted) ||
    styles.some((s) => s.id !== editingId && s.name.trim().toLowerCase() === wanted);
  return taken ? `A style named “${name.trim()}” already exists.` : null;
}

const instructionProblem = (instruction: string): string | null =>
  instruction.trim() ? null : "Write the instruction KEA should follow.";

type FormProps = {
  /** The style being edited, or null for a new one. */
  initial: CustomStyle | null;
  styles: CustomStyle[];
  busy: boolean;
  submitLabel: string;
  onSubmit: (name: string, instruction: string) => Promise<boolean>;
  onCancel?: () => void;
};

/** Name + instruction, shared by "add" and "edit" so they validate identically. */
const StyleForm = forwardRef<HTMLInputElement, FormProps>(function StyleForm(
  { initial, styles, busy, submitLabel, onSubmit, onCancel },
  nameRef,
) {
  const [name, setName] = useState(initial?.name ?? "");
  const [instruction, setInstruction] = useState(initial?.instruction ?? "");
  // Problems are shown once the user has typed something; a pristine form
  // that opens with a red message is nagging, not helping.
  const [touched, setTouched] = useState(false);
  const label = initial ? "Edited style" : "New style";

  const problem = nameProblem(name, styles, initial?.id) ?? instructionProblem(instruction);

  const submit = async () => {
    setTouched(true);
    if (problem) return;
    if (await onSubmit(name, instruction)) {
      if (!initial) {
        setName("");
        setInstruction("");
        setTouched(false);
      }
    }
  };

  return (
    <>
    <RowGroup aria-label={initial ? `Edit ${initial.name}` : "New style"}>
      <Row label="Name" hint="Shown in the Style menu.">
        <input
          ref={nameRef}
          className="kea-input"
          aria-label={`${label} name`}
          aria-invalid={touched && nameProblem(name, styles, initial?.id) !== null}
          value={name}
          onChange={(e) => {
            setName(e.target.value);
            setTouched(true);
          }}
          placeholder="Standup update"
        />
      </Row>
      <Row label="Instruction" hint="What KEA should do with the selected text.">
        <textarea
          className="kea-input"
          aria-label={`${label} instruction`}
          aria-invalid={touched && instructionProblem(instruction) !== null}
          value={instruction}
          onChange={(e) => {
            setInstruction(e.target.value);
            setTouched(true);
          }}
          rows={5}
          style={{ width: 320, resize: "vertical" }}
          placeholder="Rewrite the text as three short bullet points…"
        />
      </Row>
      <Row label="">
        <button type="button" className="kea-btn" onClick={() => void submit()} disabled={busy}>
          {submitLabel}
        </button>
        {onCancel && (
          <button type="button" className="kea-btn" onClick={onCancel} disabled={busy}>
            Cancel
          </button>
        )}
      </Row>
    </RowGroup>
    {touched && problem && (
      <p role="alert" style={{ margin: "8px 0 0", color: "var(--danger)", fontSize: "0.8125rem" }}>
        {problem}
      </p>
    )}
    </>
  );
});

/**
 * Your styles: the user's own rewrite styles, with add, edit and delete.
 *
 * A custom style is a name and an instruction. It is picked from the same
 * Style menu as the built-ins and runs on the Rewrite AI, so there is nothing
 * else to configure.
 */
export default function CustomStyles({ rewrite, newStyleRequest }: Props) {
  const { styles, busy } = rewrite;
  const activeId = rewrite.settings.style;
  const [editingId, setEditingId] = useState<string | null>(null);
  const nameInput = useRef<HTMLInputElement>(null);

  // "+ New style…" from the menu: take the user to the empty form. Zero is the
  // initial render, not a request.
  useEffect(() => {
    if (newStyleRequest === 0) return;
    setEditingId(null);
    nameInput.current?.scrollIntoView?.({ block: "center" });
    nameInput.current?.focus();
  }, [newStyleRequest]);

  const remove = (style: CustomStyle) => {
    // Only the style in use needs a second thought: deleting it changes what
    // the shortcut does. Any other is just a list entry.
    if (
      style.id === activeId &&
      !window.confirm(
        `“${style.name}” is the style your rewrite shortcut uses. Delete it and switch to Improve?`,
      )
    ) {
      return;
    }
    void rewrite.removeStyle(style.id);
  };

  return (
    <Section
      title="Your styles"
      note="Write your own instruction, then choose it from the Style menu like any other style."
    >
      {styles.length === 0 ? (
        <p className="kea-muted" style={{ margin: "0 0 12px" }}>
          You have not added a style yet.
        </p>
      ) : (
        <ul style={{ listStyle: "none", margin: "0 0 12px", padding: 0, display: "grid", gap: 12 }}>
          {styles.map((style) => (
            <li key={style.id}>
              {editingId === style.id ? (
                <StyleForm
                  initial={style}
                  styles={styles}
                  busy={busy}
                  submitLabel="Save style"
                  onSubmit={async (name, instruction) => {
                    const saved = await rewrite.saveStyle({ ...style, name, instruction });
                    if (saved) setEditingId(null);
                    return saved;
                  }}
                  onCancel={() => setEditingId(null)}
                />
              ) : (
                <RowGroup aria-label={style.name}>
                  <Row
                    label={style.name}
                    hint={
                      style.instruction.length > 120
                        ? `${style.instruction.slice(0, 120)}…`
                        : style.instruction
                    }
                  >
                    <button
                      type="button"
                      className="kea-btn"
                      aria-label={`Edit ${style.name}`}
                      onClick={() => setEditingId(style.id)}
                      disabled={busy}
                    >
                      Edit
                    </button>
                    <button
                      type="button"
                      className="kea-btn"
                      aria-label={`Delete ${style.name}`}
                      onClick={() => remove(style)}
                      disabled={busy}
                    >
                      Delete
                    </button>
                  </Row>
                </RowGroup>
              )}
            </li>
          ))}
        </ul>
      )}

      <p className="kea-muted" style={{ margin: "0 0 8px" }}>
        Add a style
      </p>
      <StyleForm
        ref={nameInput}
        initial={null}
        styles={styles}
        busy={busy}
        submitLabel="Add style"
        onSubmit={(name, instruction) => rewrite.addStyle(name, instruction)}
      />
    </Section>
  );
}
