// Pure host form + validation logic (tech-gui.md §4.1, Stage 4.1), kept free of
// Svelte components so it is unit-testable; `HostEditor.svelte` renders it. The
// validation mirrors the TUI's `HostForm::to_host` (crates/omnyssh/src/app/host.rs)
// so both frontends produce the same `hosts.toml` shape and error messages.

import type { FileAccessDto, HostDto, HostInputDto, MonitorModeDto } from '$lib/bindings';

/** The editable form fields — all raw text (tags are comma-separated, port a string). */
export interface HostFormFields {
  name: string;
  hostname: string;
  user: string;
  port: string;
  identityFile: string;
  password: string;
  tags: string;
  notes: string;
  monitoring: MonitorModeDto;
  /** Probe port; blank means "the host's SSH port". Only read for `tcpPort`. */
  monitorPort: string;
  /** How the Files tab connects for this host — independent of `monitoring`. */
  fileAccess: FileAccessDto;
  /** FTP login override; blank means "same as User" — only read for `ftp`/`ftps`. */
  ftpUser: string;
  /** FTP password override; blank on edit means "keep the stored value", same as `password`. */
  ftpPassword: string;
  /** FTP port override; blank means the standard FTP port (21). */
  ftpPort: string;
  /** Active (vs. the default passive) mode for the FTP data connection —
   *  a string so it binds to a themed `<Select>`, not a native checkbox. */
  ftpMode: 'passive' | 'active';
}

export function emptyForm(): HostFormFields {
  // Port pre-seeded to the SSH default; user blank (placeholder shows `root`, the
  // default the validation applies when it is left empty).
  return {
    name: '',
    hostname: '',
    user: '',
    port: '22',
    identityFile: '',
    password: '',
    tags: '',
    notes: '',
    monitoring: 'ssh',
    monitorPort: '',
    fileAccess: 'sftp',
    ftpUser: '',
    ftpPassword: '',
    ftpPort: '',
    ftpMode: 'passive'
  };
}

/** Seed the edit form from a `HostDto`. `identityFile`/`password` are intentionally
 *  blank: the DTO omits both (§3.4), so on edit they stay empty and mean "keep the
 *  stored value" — `save_host` preserves them unless the user types a new one. */
export function formFromHost(h: HostDto): HostFormFields {
  return {
    name: h.name,
    hostname: h.hostname,
    user: h.user,
    port: String(h.port),
    identityFile: '',
    password: '',
    tags: h.tags.join(', '),
    notes: h.notes ?? '',
    monitoring: h.monitoring,
    monitorPort: h.monitorPort == null ? '' : String(h.monitorPort),
    fileAccess: h.fileAccess,
    ftpUser: h.ftpUser ?? '',
    ftpPassword: '',
    ftpPort: h.ftpPort == null ? '' : String(h.ftpPort),
    ftpMode: h.ftpActive ? 'active' : 'passive'
  };
}

function splitCsv(raw: string): string[] {
  return raw
    .split(',')
    .map((t) => t.trim())
    .filter(Boolean);
}

export type HostFormResult = { ok: true; input: HostInputDto } | { ok: false; error: string };

/** Validate + build a `HostInputDto`, or return an error message. Mirrors the TUI's
 *  `to_host`: name and hostname are required; user defaults to `root` and port to `22`
 *  when blank; port must be a 1–65535 integer; identity/password/notes are trimmed and
 *  dropped to `undefined` when empty (so the wire form stays sparse, §4.1). `proxyJump`
 *  is not surfaced by the form (parity with the TUI, which sets it `None`) — `save_host`
 *  preserves any existing value across an edit. */
export function formToInput(f: HostFormFields): HostFormResult {
  const name = f.name.trim();
  if (!name) return { ok: false, error: 'Name cannot be empty' };
  const hostname = f.hostname.trim();
  if (!hostname) return { ok: false, error: 'Hostname / IP cannot be empty' };
  const user = f.user.trim() || 'root';

  const portRaw = f.port.trim();
  let port = 22;
  if (portRaw !== '') {
    // Digits with an optional leading `+`, matching Rust's `u16::parse` (which accepts
    // `+22` but no `-`/decimal/hex/exponent); the range guard covers 0 and overflow.
    if (!/^\+?\d+$/.test(portRaw) || Number(portRaw) < 1 || Number(portRaw) > 65535) {
      return { ok: false, error: `Port must be a number between 1 and 65535, got '${portRaw}'` };
    }
    port = Number(portRaw);
  }

  // Only meaningful for a reachability host; an SSH host never carries a probe port.
  let monitorPort: number | undefined;
  const monitorPortRaw = f.monitorPort.trim();
  if (f.monitoring === 'tcpPort' && monitorPortRaw !== '') {
    if (!/^\+?\d+$/.test(monitorPortRaw) || Number(monitorPortRaw) < 1 || Number(monitorPortRaw) > 65535) {
      return {
        ok: false,
        error: `Probe port must be a number between 1 and 65535, got '${monitorPortRaw}'`
      };
    }
    monitorPort = Number(monitorPortRaw);
  }

  // Only meaningful for ftp/ftps; blank means "use the standard FTP port (21)".
  let ftpPort: number | undefined;
  const ftpPortRaw = f.ftpPort.trim();
  if (ftpPortRaw !== '') {
    if (!/^\+?\d+$/.test(ftpPortRaw) || Number(ftpPortRaw) < 1 || Number(ftpPortRaw) > 65535) {
      return {
        ok: false,
        error: `FTP Port must be a number between 1 and 65535, got '${ftpPortRaw}'`
      };
    }
    ftpPort = Number(ftpPortRaw);
  }

  const identityFile = f.identityFile.trim();
  const password = f.password.trim();
  const notes = f.notes.trim();
  const ftpUser = f.ftpUser.trim();
  const ftpPassword = f.ftpPassword.trim();
  const tags = splitCsv(f.tags);
  return {
    ok: true,
    input: {
      name,
      hostname,
      user,
      port,
      identityFile: identityFile || undefined,
      password: password || undefined,
      tags,
      notes: notes || undefined,
      monitoring: f.monitoring,
      monitorPort,
      fileAccess: f.fileAccess,
      ftpUser: ftpUser || undefined,
      ftpPassword: ftpPassword || undefined,
      ftpPort,
      ftpActive: f.ftpMode === 'active'
    }
  };
}
