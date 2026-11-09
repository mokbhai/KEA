import { REWRITE_MODES, type CustomStyle } from "../api";

/**
 * The `<option>`s of a style `<select>`: the built-ins, then the user's own.
 *
 * One list for every picker that names a style (the Rewrite page, an app
 * rule), so a style added in one place appears in the other without either
 * remembering to ask.
 */
export default function StyleOptions({ styles }: { styles: CustomStyle[] }) {
  return (
    <>
      {REWRITE_MODES.map((m) => (
        <option key={m.value} value={m.value}>
          {m.label}
        </option>
      ))}
      {styles.map((s) => (
        <option key={s.id} value={s.id}>
          {s.name}
        </option>
      ))}
    </>
  );
}
