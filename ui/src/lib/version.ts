/**
 * The version as the sidebar shows it.
 *
 * Builds are versioned in semver (`0.5.6-beta.2`) because Cargo and Tauri
 * require it; the owner reads and writes betas as `0.5.6b2`. Releases pass
 * through unchanged.
 */
export function displayVersion(version: string): string {
  const beta = /^(\d+\.\d+\.\d+)-beta\.(\d+)$/.exec(version);
  return beta ? `${beta[1]}b${beta[2]}` : version;
}
