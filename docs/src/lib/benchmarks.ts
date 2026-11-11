// The benchmark suites the docs show. Each suite is a directory `tools/bench/<suite>/` with a
// `suite.toml` that names and describes its programs and, once measured, a `results.json` that
// `just bench <suite> --write` regenerates. Pages, the landing page, the Markdown export and
// `tools/bench/readme.py` render only these files, so a number never drifts from its measurement.
// The runner owns every judgement about the data: which programs are too short, which typed
// programs were refused, and the geometric means. This module renders them and computes nothing.
import { parse } from 'smol-toml';

const manifests = import.meta.glob('../../../tools/bench/*/suite.toml', { query: '?raw', import: 'default', eager: true }) as Record<string, string>;
const resultFiles = import.meta.glob('../../../tools/bench/*/results.json', { import: 'default', eager: true }) as Record<string, unknown>;
const sourceFiles = import.meta.glob('../../../tools/bench/**/*.R', { query: '?raw', import: 'default', eager: true }) as Record<string, string>;

export const repository = 'https://github.com/felix-andreas/ry/blob/main';

// The order of the summary, the page and the README.
const suiteOrder = ['public', 'packages', 'kernels', 'applications'];

export type Interval = { median: number; low?: number; high?: number };
export type Flag = { kind: string; engine?: string; reason?: string };
export type TestCounts = { passed: number; failed: number; errors: number };
export type Group = { id: string; name: string; description?: string; url?: string; license?: string };
export type Meta = {
  date: string;
  ryCommit?: string;
  gnuR: { version: string; blas?: string };
  machine: { cpu: string; cores?: number; kernel?: string };
  rounds: number;
  seed?: number;
  command?: string;
  tooShortSeconds?: number;
};
export type Program = {
  id: string;
  display: string;
  summary?: string;
  source?: string;
  typedSource?: string;
  group?: string;
  version?: string;
  // Absent for a program the suite defines but the results file does not hold.
  measurement?: Measurement;
};
export type Measurement = {
  seconds: Record<string, number>;
  ratio: Record<string, Interval>;
  peakMib?: Record<string, number>;
  tests?: Record<string, TestCounts>;
  flags: Flag[];
};
export type Results = {
  path: string;
  engines: string[];
  geomean: Record<string, Interval & { programs: number }>;
  meta: Meta;
};
export type Suite = {
  id: string;
  name: string;
  summary: string;
  directory: string;
  harness?: string;
  groups: Group[];
  programs: Program[];
  results?: Results;
};

// The engine every ratio divides by: a ratio above 1x means the engine ran faster than GNU R.
export const baseline = 'gnu_r';

const engineLabels: Record<string, string> = {
  gnu_r: 'GNU R',
  ry: 'ry',
  ry_typed: 'ry, typed and strict',
  ry_nojit: 'ry, JIT off',
};

export function engineLabel(engine: string): string {
  return engineLabels[engine] ?? engine;
}

export const suites: Suite[] = loadSuites();

export function suite(id: string): Suite | undefined {
  return suites.find((candidate) => candidate.id === id);
}

export function measuredSuites(): Suite[] {
  return suites.filter((candidate) => candidate.results !== undefined);
}

// The engines a suite compares against GNU R, in the order of its results file.
export function comparedEngines(suite: Suite): string[] {
  return (suite.results?.engines ?? []).filter((engine) => engine !== baseline);
}

export function measuredPrograms(suite: Suite): (Program & { measurement: Measurement })[] {
  return suite.programs.filter((program): program is Program & { measurement: Measurement } => program.measurement !== undefined);
}

export function sourceUrl(path: string): string {
  return `${repository}/${path}`;
}

export function sourceText(path: string): string | undefined {
  return sourceFiles[`../../../${path}`];
}

// Whole numbers from 10x up, where a decimal adds nothing, and two decimals below, where 0.94x
// against 1.02x is the point.
export function formatRatio(ratio: number): string {
  return ratio >= 10 ? `${ratio.toFixed(0)}x` : `${ratio.toFixed(2)}x`;
}

export function formatInterval(interval: Interval): string {
  if (interval.low === undefined || interval.high === undefined) return formatRatio(interval.median);
  return `${formatRatio(interval.median)} (${formatRatio(interval.low)} to ${formatRatio(interval.high)})`;
}

export function seconds(value: number): string {
  return `${value.toFixed(3)} s`;
}

export function mebibytes(value: number): string {
  return `${value.toFixed(0)} MiB`;
}

// A flag in words, for the label under a bar and for a table cell.
export function flagText(flag: Flag): string {
  const engine = flag.engine === undefined ? '' : ` on ${engineLabel(flag.engine)}`;
  if (flag.kind === 'too_short') return `too short${engine}`;
  if (flag.kind === 'typed_refused') return `typed version refused${flag.reason === undefined ? '' : `: ${flag.reason}`}`;
  if (flag.kind === 'wide_interval') return `wide interval${engine}`;
  return `${flag.kind.replaceAll('_', ' ')}${engine}${flag.reason === undefined ? '' : `: ${flag.reason}`}`;
}

// A ratio rests on the engine's time and on GNU R's, so a flag on either one marks it.
export function ratioFlags(measurement: Measurement, engine: string): Flag[] {
  return measurement.flags.filter((flag) => flag.engine === undefined || flag.engine === engine || flag.engine === baseline);
}

// Whether a program is too short to count: GNU R, whose time measures the work, finished it under
// the floor. A short time on a ry engine is the speed-up being measured, so it is flagged but counts.
export function isTooShort(measurement: Measurement): boolean {
  return measurement.flags.some((flag) => flag.kind === 'too_short' && (flag.engine === undefined || flag.engine === baseline));
}

export function flagNotes(programs: Program[], floor = 0.1): string[] {
  const flags = programs.flatMap((program) => program.measurement?.flags ?? []);
  const kinds = new Set(flags.map((flag) => flag.kind));
  const notes: string[] = [];
  if (programs.some((program) => program.measurement !== undefined && isTooShort(program.measurement))) {
    notes.push(`A program marked too short on GNU R finishes there in under ${floor} s, which is too little work for a stable ratio, so every geometric mean leaves it out.`);
  }
  if (flags.some((flag) => flag.kind === 'too_short' && flag.engine !== undefined && flag.engine !== baseline)) {
    notes.push(`A time marked too short on a ry engine is under ${floor} s. That speed is what the suite measures, so it counts in the geometric mean.`);
  }
  if (kinds.has('typed_refused')) notes.push('A typed version the compiler refused has no typed time; the flag gives the reason.');
  if (kinds.has('wide_interval')) notes.push("A program marked wide interval varied so much between rounds that its ratio's interval is wide; read its median with care.");
  return notes;
}

// When, where and how a suite was measured, printed under each of its charts and tables.
export function suiteMetadata(suite: Suite): string {
  const results = suite.results;
  if (results === undefined) return '';
  const { meta } = results;
  const commit = meta.ryCommit === undefined ? '' : ` at ry ${shortCommit(meta.ryCommit)}`;
  return `Measured on ${meta.date.slice(0, 10)}${commit} with GNU R ${meta.gnuR.version} on ${machine(meta)}; ${meta.rounds} paired rounds.`;
}

// A commit as its first nine hex digits, keeping a marker such as `-dirty` that follows the hash.
export function shortCommit(commit: string): string {
  return commit.replace(/^([0-9a-f]{9})[0-9a-f]*/, '$1');
}

export function machine(meta: Meta): string {
  const parts = [meta.machine.cpu];
  if (meta.machine.cores !== undefined) parts.push(`${meta.machine.cores} cores`);
  if (meta.machine.kernel !== undefined) parts.push(meta.machine.kernel);
  return parts.join(', ');
}

// One row per measured suite: what it is and ry's geometric-mean speed against GNU R, untyped and
// typed in strict mode. tools/bench/readme.py writes the same rows into the READMEs.
export const summaryColumns = ['Suite', 'What it is', 'ry vs GNU R', 'ry typed'];

export function summaryRows(): string[][] {
  return measuredSuites().map((suite) => {
    const geomean = suite.results?.geomean ?? {};
    const untyped = geomean.ry;
    const typed = geomean.ry_typed;
    return [
      suite.name,
      `${measuredPrograms(suite).length} ${suite.summary}`,
      untyped === undefined ? 'not measured' : formatRatio(untyped.median),
      typed === undefined ? 'not typed' : formatRatio(typed.median),
    ];
  });
}

export const summaryNote = 'Geometric means of the speed-ups over GNU R; above 1x means ry is faster.';

export function summaryMarkdown(): string {
  return [
    `| ${summaryColumns.join(' | ')} |`,
    '| --- | --- | ---: | ---: |',
    ...summaryRows().map((row) => `| ${row.join(' | ')} |`),
    '',
    summaryNote,
    '',
    ...measuredSuites().map((suite) => `- ${suite.name}: ${lowercaseFirst(suiteMetadata(suite))}`),
  ].join('\n');
}

// A bar chart of speed relative to GNU R: one row per program, one bar per engine, on a logarithmic
// axis centred on 1x, so 10x faster and 10x slower are bars of the same length.
export type ChartBar = { engine: string; ratio?: Interval; faded: boolean; title: string };
export type ChartRow = { label: string; id?: string; href?: string; flags: string[]; bars: ChartBar[]; summary?: boolean };
export type Chart = { caption: string; engines: string[]; rows: ChartRow[]; notes: string[]; metadata: string };

// One chart per group, or one for the whole suite when it has no groups.
export function suiteCharts(suite: Suite): Chart[] {
  const engines = comparedEngines(suite);
  const programs = measuredPrograms(suite);
  const groups = suite.groups.length === 0 ? [undefined] : suite.groups;
  const charts: Chart[] = groups.flatMap((group) => {
    const members = group === undefined ? programs : programs.filter((program) => program.group === group.id);
    if (members.length === 0) return [];
    const rows: ChartRow[] = members
      .map((program) => chartRow(program, engines))
      .sort((a, b) => (b.bars[0]?.ratio?.median ?? 0) - (a.bars[0]?.ratio?.median ?? 0));
    return [{
      caption: group === undefined ? suite.name : `${suite.name}: ${group.name}`,
      engines,
      rows,
      notes: flagNotes(members, suite.results?.meta.tooShortSeconds),
      metadata: suiteMetadata(suite),
    }];
  });
  // The geometric mean spans the whole suite, so it closes the last chart.
  charts.at(-1)?.rows.push(geomeanRow(suite, engines, groups.length > 1));
  return charts;
}

function chartRow(program: Program & { measurement: Measurement }, engines: string[]): ChartRow {
  const { measurement } = program;
  const gnuR = measurement.seconds[baseline];
  return {
    label: program.display,
    id: program.id === program.display ? undefined : program.id,
    href: program.source === undefined ? undefined : sourceUrl(program.source),
    flags: measurement.flags.map(flagText),
    bars: engines.map((engine) => {
      const ratio = measurement.ratio[engine];
      const time = measurement.seconds[engine];
      const title = ratio === undefined
        ? `${program.display}, ${engineLabel(engine)}: not measured`
        : `${program.display}, ${engineLabel(engine)}: ${formatInterval(ratio)} the speed of GNU R` +
          (time === undefined || gnuR === undefined ? '' : `, ${seconds(time)} against ${seconds(gnuR)}`);
      return { engine, ratio, faded: isTooShort(measurement), title };
    }),
  };
}

function geomeanRow(suite: Suite, engines: string[], grouped: boolean): ChartRow {
  const geomean = suite.results?.geomean ?? {};
  return {
    label: grouped ? 'geometric mean of all groups' : 'geometric mean',
    flags: [],
    summary: true,
    bars: engines.map((engine) => {
      const value = geomean[engine];
      return {
        engine,
        ratio: value,
        faded: false,
        title: value === undefined ? `${engineLabel(engine)}: no geometric mean` : `${engineLabel(engine)}: ${formatInterval(value)} over ${value.programs} programs`,
      };
    }),
  };
}

export function chartMarkdown(suite: Suite): string {
  if (suite.results === undefined) return '';
  return suiteCharts(suite)
    .map((chart) =>
      [
        `${chart.caption}, speed-up over GNU R:`,
        '',
        `| program | ${chart.engines.map(engineLabel).join(' | ')} |`,
        `| --- |${chart.engines.map(() => ' ---: |').join('')}`,
        ...chart.rows.map((row) => {
          const label = `${row.label}${row.id === undefined ? '' : ` (\`${row.id}\`)`}${row.flags.length === 0 ? '' : `, ${row.flags.join(', ')}`}`;
          return `| ${label} | ${row.bars.map((bar) => (bar.ratio === undefined ? 'n/a' : formatInterval(bar.ratio))).join(' | ')} |`;
        }),
        '',
        [...chart.notes, chart.metadata].join(' '),
      ].join('\n'),
    )
    .join('\n\n');
}

// The measurement as tables: seconds and ratios, peak memory where the runner recorded it, and the
// test counts of a suite of test suites.
export type TableView = { label: string; columns: string[]; rows: string[][]; note?: string };

export function tableViews(suite: Suite): TableView[] {
  const engines = suite.results?.engines ?? [];
  const compared = comparedEngines(suite);
  const programs = measuredPrograms(suite);
  const name = (program: Program) => `${program.display}${program.version === undefined ? '' : ` ${program.version}`}`;
  const views: TableView[] = [{
    label: 'Time',
    columns: ['program', ...engines.map(engineLabel), ...compared.map((engine) => `${engineLabel(engine)}, speed-up`)],
    rows: programs.map((program) => [
      name(program),
      ...engines.map((engine) => {
        const value = program.measurement.seconds[engine];
        return value === undefined ? 'n/a' : seconds(value);
      }),
      ...compared.map((engine) => {
        const ratio = program.measurement.ratio[engine];
        const flags = ratioFlags(program.measurement, engine).map(flagText);
        return `${ratio === undefined ? 'n/a' : formatInterval(ratio)}${flags.length === 0 ? '' : ` (${flags.join(', ')})`}`;
      }),
    ]),
    note: 'Each time is the median over the rounds. Each speed-up is the median of the per-round ratios, with its 95% interval in parentheses.',
  }];
  if (programs.some((program) => program.measurement.peakMib !== undefined)) {
    views.push({
      label: 'Memory',
      columns: ['program', ...engines.map(engineLabel)],
      rows: programs.map((program) => [
        name(program),
        ...engines.map((engine) => {
          const value = program.measurement.peakMib?.[engine];
          return value === undefined ? 'n/a' : mebibytes(value);
        }),
      ]),
      note: "Peak memory is the peak resident set size of the whole process, R's startup included.",
    });
  }
  if (programs.some((program) => program.measurement.tests !== undefined)) {
    views.push({
      label: 'Tests',
      columns: ['package', ...engines.map((engine) => `${engineLabel(engine)}: passed, failed, errors`)],
      rows: programs.map((program) => [
        name(program),
        ...engines.map((engine) => {
          const counts = program.measurement.tests?.[engine];
          return counts === undefined ? 'n/a' : `${counts.passed}, ${counts.failed}, ${counts.errors}`;
        }),
      ]),
    });
  }
  return views;
}

export function tableMarkdown(suite: Suite, headingLevel: number): string {
  if (suite.results === undefined) return '';
  const heading = '#'.repeat(Math.min(headingLevel, 6));
  return [
    ...tableViews(suite).map((view) =>
      [
        `${heading} ${view.label}`,
        '',
        `| ${view.columns.join(' | ')} |`,
        `| --- |${view.columns.slice(1).map(() => ' ---: |').join('')}`,
        ...view.rows.map((row) => `| ${row.join(' | ')} |`),
        ...(view.note === undefined ? [] : ['', view.note]),
      ].join('\n'),
    ),
    suiteMetadata(suite),
  ].join('\n\n');
}

// A program's code without its leading comment, which the suite's summary already says in words.
export function programCode(path: string | undefined): string | undefined {
  if (path === undefined) return undefined;
  const text = sourceText(path);
  if (text === undefined) return undefined;
  const lines = text.split('\n');
  const start = lines.findIndex((line) => !line.startsWith('#'));
  return lines.slice(Math.max(start, 0)).join('\n').trim();
}

// A program's headline number: ry's speed-up on the untyped program.
export function programRatio(program: Program): string {
  const measurement = program.measurement;
  if (measurement === undefined) return 'not in the committed results';
  const ratio = measurement.ratio.ry;
  if (ratio === undefined) return '';
  return `${formatRatio(ratio.median)}${isTooShort(measurement) ? ' (too short)' : ''}`;
}

// Programs listed by group, each group with its upstream source and licence.
export function programGroups(suite: Suite): { group?: Group; programs: Program[] }[] {
  if (suite.groups.length === 0) return [{ programs: suite.programs }];
  return suite.groups.map((group) => ({ group, programs: suite.programs.filter((program) => program.group === group.id) }));
}

export function programsMarkdown(suite: Suite, headingLevel: number): string {
  const heading = '#'.repeat(Math.min(headingLevel, 6));
  return programGroups(suite)
    .map(({ group, programs }) => {
      const intro = group === undefined
        ? []
        : [`${heading} ${group.name}`, '', [group.description, group.url && `Source: ${group.url}`, group.license && `Licence: ${group.license}.`].filter(Boolean).join(' '), ''];
      const entries = programs.map((program) => {
        const lines = [`- **${program.display}** ([\`${program.id}\`](${sourceUrl(program.source ?? suite.directory)}), ${programRatio(program)}): ${program.summary ?? ''}`];
        const untyped = programCode(program.source);
        const typed = programCode(program.typedSource);
        if (typed !== undefined && untyped !== undefined) {
          lines.push('', '  Plain R:', '', '  ```r', ...untyped.split('\n').map((line) => `  ${line}`), '  ```', '', '  Typed:', '', '  ```r', ...typed.split('\n').map((line) => `  ${line}`), '  ```');
        }
        return lines.join('\n');
      });
      return [...intro, ...entries].join('\n');
    })
    .join('\n\n');
}

// How each measured suite was run, as rows of one table with a column per suite.
export function methodRows(): { columns: string[]; rows: string[][] } {
  const measured = measuredSuites();
  const meta = (suite: Suite) => suite.results?.meta;
  const row = (label: string, cell: (suite: Suite) => string) => [label, ...measured.map(cell)];
  return {
    columns: ['', ...measured.map((suite) => suite.name)],
    rows: [
      row('Command', (suite) => (meta(suite)?.command === undefined ? '' : `\`${meta(suite)?.command}\``)),
      row('Results file', (suite) => `\`${suite.results?.path ?? ''}\``),
      row('Measured', (suite) => `${meta(suite)?.date.slice(0, 10) ?? ''}${meta(suite)?.ryCommit === undefined ? '' : ` at ry ${shortCommit(meta(suite)?.ryCommit ?? '')}`}`),
      row('Machine', (suite) => { const value = meta(suite); return value === undefined ? '' : machine(value); }),
      row('GNU R', (suite) => `${meta(suite)?.gnuR.version ?? ''}${meta(suite)?.gnuR.blas === undefined ? '' : `, ${meta(suite)?.gnuR.blas}`}`),
      row('Engines', (suite) => (suite.results?.engines ?? []).map(engineLabel).join('; ')),
      row('Samples', (suite) => `${meta(suite)?.rounds} paired rounds${meta(suite)?.seed === undefined ? '' : `, shuffle seed ${meta(suite)?.seed}`}`),
    ],
  };
}

export function methodMarkdown(): string {
  const { columns, rows } = methodRows();
  return [
    `| ${columns.join(' | ')} |`,
    `| --- |${columns.slice(1).map(() => ' --- |').join('')}`,
    ...rows.map((row) => `| ${row.join(' | ')} |`),
  ].join('\n');
}

function lowercaseFirst(text: string): string {
  return text.slice(0, 1).toLowerCase() + text.slice(1);
}

type Manifest = {
  id: string;
  name: string;
  summary: string;
  harness?: string;
  groups?: Group[];
  programs?: { id: string; display: string; summary?: string; source?: string; typed_source?: string; group?: string; version?: string }[];
};

type RawResults = {
  schema: number;
  // Each engine's id mapped to the label the runner printed, in the order the runner ran them.
  engines: Record<string, string>;
  meta: {
    date: string;
    ry_commit?: string | null;
    gnu_r: { version: string; blas?: string | null };
    machine: { cpu: string; cores?: number | null; kernel?: string | null };
    rounds: number;
    seed?: number | null;
    command?: string | null;
    too_short_seconds?: number | null;
  };
  programs: {
    id: string;
    display?: string;
    seconds: Record<string, number>;
    ratio: Record<string, Interval>;
    peak_mib?: Record<string, number>;
    tests?: Record<string, TestCounts>;
    flags?: Flag[];
  }[];
  geomean: Record<string, Interval & { programs: number }>;
};

function loadSuites(): Suite[] {
  const loaded = Object.entries(manifests).map(([path, text]) => {
    const directory = path.replace(/^\.\.\/\.\.\/\.\.\//, '').replace(/\/suite\.toml$/, '');
    const manifest = parse(text) as unknown as Manifest;
    const raw = resultFiles[`../../../${directory}/results.json`];
    const results = raw === undefined ? undefined : normalize(manifest.id, raw);
    const byId = new Map((results?.programs ?? []).map((program) => [program.id, program]));
    const programs: Program[] = (manifest.programs ?? []).map((program) => ({
      id: program.id,
      display: program.display,
      summary: program.summary,
      source: program.source,
      typedSource: program.typed_source,
      group: program.group,
      version: program.version,
      measurement: measurement(byId.get(program.id)),
    }));
    // A measured program the manifest does not name still shows, under the name the results give it.
    for (const program of results?.programs ?? []) {
      if (!programs.some((known) => known.id === program.id)) {
        programs.push({ id: program.id, display: program.display ?? program.id, measurement: measurement(program) });
      }
    }
    return {
      id: manifest.id,
      name: manifest.name,
      summary: manifest.summary,
      directory,
      harness: manifest.harness,
      groups: manifest.groups ?? [],
      programs,
      results: results === undefined
        ? undefined
        : { path: `${directory}/results.json`, engines: Object.keys(results.engines), geomean: results.geomean, meta: meta(results.meta) },
    } satisfies Suite;
  });
  for (const [index, entry] of loaded.entries()) {
    const twin = loaded.findIndex((other) => other.id === entry.id);
    if (twin !== index) throw new Error(`benchmark suite \`${entry.id}\` is defined by both ${loaded[twin].directory} and ${entry.directory}`);
  }
  const rank = (id: string) => (suiteOrder.includes(id) ? suiteOrder.indexOf(id) : suiteOrder.length);
  return loaded.sort((a, b) => rank(a.id) - rank(b.id) || a.id.localeCompare(b.id));
}

function measurement(program: RawResults['programs'][number] | undefined): Measurement | undefined {
  if (program === undefined) return undefined;
  return { seconds: program.seconds, ratio: program.ratio, peakMib: program.peak_mib, tests: program.tests, flags: program.flags ?? [] };
}

function meta(raw: RawResults['meta']): Meta {
  return {
    // A runner written in Python stores an absent value as null, which reads here as absent.
    date: raw.date,
    ryCommit: raw.ry_commit ?? undefined,
    gnuR: { version: raw.gnu_r.version, blas: raw.gnu_r.blas ?? undefined },
    machine: { cpu: raw.machine.cpu, cores: raw.machine.cores ?? undefined, kernel: raw.machine.kernel ?? undefined },
    rounds: raw.rounds,
    seed: raw.seed ?? undefined,
    command: raw.command ?? undefined,
    tooShortSeconds: raw.too_short_seconds ?? undefined,
  };
}

function normalize(id: string, raw: unknown): RawResults {
  const value = raw as { schema?: number };
  if (value.schema === 1) return raw as RawResults;
  throw new Error(`benchmark suite \`${id}\`: results.json has schema ${value.schema}, which the docs do not read`);
}
