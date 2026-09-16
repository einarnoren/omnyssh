import { describe, expect, it } from 'vitest';
import type { HostDto } from '$lib/bindings';
import { emptyForm, formFromHost, formToInput, type HostFormFields } from './hostForm';

function fields(partial: Partial<HostFormFields>): HostFormFields {
  return { ...emptyForm(), ...partial };
}

function host(partial: Partial<HostDto>): HostDto {
  return {
    name: 'web',
    hostname: 'web.example.com',
    user: 'deploy',
    port: 22,
    tags: [],
    source: 'manual',
    hasKey: false,
    monitoring: 'ssh',
    fileAccess: 'sftp',
    ftpActive: false,
    ...partial
  };
}

describe('formToInput — mirrors the TUI to_host', () => {
  it('rejects an empty name', () => {
    expect(formToInput(fields({ name: '  ', hostname: 'h' }))).toEqual({
      ok: false,
      error: 'Name cannot be empty'
    });
  });

  it('rejects an empty hostname', () => {
    expect(formToInput(fields({ name: 'n', hostname: '  ' }))).toEqual({
      ok: false,
      error: 'Hostname / IP cannot be empty'
    });
  });

  it('defaults an empty user to root', () => {
    const r = formToInput(fields({ name: 'n', hostname: 'h', user: '  ' }));
    expect(r.ok && r.input.user).toBe('root');
  });

  it('defaults an empty port to 22', () => {
    const r = formToInput(fields({ name: 'n', hostname: 'h', port: '' }));
    expect(r.ok && r.input.port).toBe(22);
  });

  it('parses a valid port', () => {
    const r = formToInput(fields({ name: 'n', hostname: 'h', port: '2222' }));
    expect(r.ok && r.input.port).toBe(2222);
  });

  it('accepts the max port 65535', () => {
    const r = formToInput(fields({ name: 'n', hostname: 'h', port: '65535' }));
    expect(r.ok && r.input.port).toBe(65535);
  });

  it('accepts an optional leading + (parity with Rust u16::parse)', () => {
    const r = formToInput(fields({ name: 'n', hostname: 'h', port: '+22' }));
    expect(r.ok && r.input.port).toBe(22);
  });

  it.each(['0', '65536', '-1', '22.5', 'abc', '2e3', '0x10', '99999'])(
    'rejects an invalid port %s',
    (port) => {
      const r = formToInput(fields({ name: 'n', hostname: 'h', port }));
      expect(r).toEqual({
        ok: false,
        error: `Port must be a number between 1 and 65535, got '${port}'`
      });
    }
  );

  it('trims name/hostname and splits tags, dropping blanks', () => {
    const r = formToInput(fields({ name: '  web ', hostname: ' 10.0.0.1 ', tags: 'prod, , db ,' }));
    expect(r.ok && r.input.name).toBe('web');
    expect(r.ok && r.input.hostname).toBe('10.0.0.1');
    expect(r.ok && r.input.tags).toEqual(['prod', 'db']);
  });

  it('drops blank optionals to undefined so the wire form stays sparse', () => {
    const r = formToInput(fields({ name: 'n', hostname: 'h', identityFile: '  ', password: '', notes: ' ' }));
    expect(r.ok && r.input.identityFile).toBeUndefined();
    expect(r.ok && r.input.password).toBeUndefined();
    expect(r.ok && r.input.notes).toBeUndefined();
  });

  it('keeps identity/password/notes when provided', () => {
    const r = formToInput(
      fields({ name: 'n', hostname: 'h', identityFile: '~/.ssh/id', password: 's3cret', notes: 'prod box' })
    );
    expect(r.ok && r.input.identityFile).toBe('~/.ssh/id');
    expect(r.ok && r.input.password).toBe('s3cret');
    expect(r.ok && r.input.notes).toBe('prod box');
  });

  it('always emits a tags array (empty, not undefined)', () => {
    const r = formToInput(fields({ name: 'n', hostname: 'h', tags: '' }));
    expect(r.ok && r.input.tags).toEqual([]);
  });
});

describe('formFromHost', () => {
  it('seeds the editable fields and leaves secrets blank (the DTO omits them)', () => {
    const f = formFromHost(host({ name: 'db', hostname: '10.0.0.2', user: 'root', port: 2200, tags: ['prod', 'db'], notes: 'primary' }));
    expect(f.name).toBe('db');
    expect(f.hostname).toBe('10.0.0.2');
    expect(f.user).toBe('root');
    expect(f.port).toBe('2200');
    expect(f.tags).toBe('prod, db');
    expect(f.notes).toBe('primary');
    // Backend-only fields are never shown — blank means "keep the stored value".
    expect(f.identityFile).toBe('');
    expect(f.password).toBe('');
  });

  it('round-trips the observable fields back through formToInput', () => {
    const original = host({ name: 'db', hostname: '10.0.0.2', user: 'root', port: 2200, tags: ['ops'], notes: 'x' });
    const r = formToInput(formFromHost(original));
    expect(r.ok && r.input).toEqual({
      name: 'db',
      hostname: '10.0.0.2',
      user: 'root',
      port: 2200,
      identityFile: undefined,
      password: undefined,
      tags: ['ops'],
      notes: 'x',
      monitoring: 'ssh',
      monitorPort: undefined,
      fileAccess: 'sftp',
      ftpActive: false
    });
  });
});

describe('formToInput — monitoring mode', () => {
  it('defaults to ssh and sends no probe port', () => {
    const result = formToInput({ ...emptyForm(), name: 'web', hostname: '10.0.0.1' });
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.input.monitoring).toBe('ssh');
      expect(result.input.monitorPort).toBeUndefined();
    }
  });

  it('carries a probe port for a reachability host', () => {
    const result = formToInput({
      ...emptyForm(),
      name: 'fw',
      hostname: '10.0.0.9',
      monitoring: 'tcpPort',
      monitorPort: '8443'
    });
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.input.monitoring).toBe('tcpPort');
      expect(result.input.monitorPort).toBe(8443);
    }
  });

  it('falls back to the host port when the probe port is blank', () => {
    const result = formToInput({ ...emptyForm(), name: 'fw', hostname: '10.0.0.9', monitoring: 'tcpPort' });
    expect(result.ok).toBe(true);
    if (result.ok) expect(result.input.monitorPort).toBeUndefined();
  });

  it('rejects an out-of-range probe port', () => {
    for (const monitorPort of ['0', '99999', 'ssh']) {
      const result = formToInput({
        ...emptyForm(),
        name: 'fw',
        hostname: '10.0.0.9',
        monitoring: 'tcpPort',
        monitorPort
      });
      expect(result.ok).toBe(false);
    }
  });

  it('ignores a probe port left over from switching back to ssh', () => {
    const result = formToInput({
      ...emptyForm(),
      name: 'web',
      hostname: '10.0.0.1',
      monitoring: 'ssh',
      monitorPort: '8443'
    });
    expect(result.ok).toBe(true);
    if (result.ok) expect(result.input.monitorPort).toBeUndefined();
  });

  it('round-trips a reachability host through the edit form', () => {
    const fields = formFromHost(host({ monitoring: 'tcpPort', monitorPort: 8443 }));
    expect(fields.monitoring).toBe('tcpPort');
    expect(fields.monitorPort).toBe('8443');
  });
});

describe('formToInput — file access', () => {
  it('defaults to sftp', () => {
    const result = formToInput({ ...emptyForm(), name: 'web', hostname: '10.0.0.1' });
    expect(result.ok).toBe(true);
    if (result.ok) expect(result.input.fileAccess).toBe('sftp');
  });

  it.each(['ftp', 'ftps', 'none'] as const)('carries a non-default choice (%s)', (fileAccess) => {
    const result = formToInput({ ...emptyForm(), name: 'nas', hostname: '10.0.0.5', fileAccess });
    expect(result.ok).toBe(true);
    if (result.ok) expect(result.input.fileAccess).toBe(fileAccess);
  });

  it('round-trips a non-default host through the edit form', () => {
    const fields = formFromHost(host({ fileAccess: 'ftp' }));
    expect(fields.fileAccess).toBe('ftp');
    const r = formToInput(fields);
    expect(r.ok && r.input.fileAccess).toBe('ftp');
  });
});

describe('formToInput — FTP overrides', () => {
  it('leaves overrides undefined when blank', () => {
    const result = formToInput({ ...emptyForm(), name: 'nas', hostname: '10.0.0.5', fileAccess: 'ftp' });
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.input.ftpUser).toBeUndefined();
      expect(result.input.ftpPassword).toBeUndefined();
      expect(result.input.ftpPort).toBeUndefined();
    }
  });

  it('carries a provided override', () => {
    const result = formToInput({
      ...emptyForm(),
      name: 'nas',
      hostname: '10.0.0.5',
      fileAccess: 'ftp',
      ftpUser: 'ftpuser',
      ftpPassword: 'ftppass',
      ftpPort: '2121'
    });
    expect(result.ok).toBe(true);
    if (result.ok) {
      expect(result.input.ftpUser).toBe('ftpuser');
      expect(result.input.ftpPassword).toBe('ftppass');
      expect(result.input.ftpPort).toBe(2121);
    }
  });

  it('rejects an out-of-range FTP port', () => {
    const result = formToInput({
      ...emptyForm(),
      name: 'nas',
      hostname: '10.0.0.5',
      fileAccess: 'ftp',
      ftpPort: '99999'
    });
    expect(result).toEqual({ ok: false, error: "FTP Port must be a number between 1 and 65535, got '99999'" });
  });

  it('seeds ftpUser/ftpPort from the host but leaves ftpPassword blank (secret)', () => {
    const fields = formFromHost(host({ fileAccess: 'ftp', ftpUser: 'ftpuser', ftpPort: 2121 }));
    expect(fields.ftpUser).toBe('ftpuser');
    expect(fields.ftpPort).toBe('2121');
    expect(fields.ftpPassword).toBe('');
  });

  it('defaults to passive mode', () => {
    const result = formToInput({ ...emptyForm(), name: 'nas', hostname: '10.0.0.5', fileAccess: 'ftp' });
    expect(result.ok && result.input.ftpActive).toBe(false);
  });

  it('carries active mode through to the input', () => {
    const result = formToInput({
      ...emptyForm(),
      name: 'nas',
      hostname: '10.0.0.5',
      fileAccess: 'ftp',
      ftpMode: 'active'
    });
    expect(result.ok && result.input.ftpActive).toBe(true);
  });

  it('round-trips active mode through the edit form', () => {
    const fields = formFromHost(host({ fileAccess: 'ftp', ftpActive: true }));
    expect(fields.ftpMode).toBe('active');
    const r = formToInput(fields);
    expect(r.ok && r.input.ftpActive).toBe(true);
  });
});
