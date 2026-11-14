// The benchmark suites the docs show. Each suite is a directory `tools/bench/<suite>/` with a
// `suite.toml` that names and describes its programs and, once measured, a `results.json` that
// `just bench <suite> --write` regenerates. Pages, the landing page, the Markdown export and
// `tools/bench/readme.py` render only these files, so a number never drifts from its measurement.
// The runner owns every judgement about the data: which programs are too short, which typed
// programs were refused, and the geometric means. This module renders them and computes nothing.
import { parse } from 'smol-toml';

const manifests = import.meta.glob('../../../tools/bench/*/suite.toml', { query: '?raw', import: 'default', eager: true }) as Record<string, string>;
const resultFiles = import.meta.glob('../../../tools/bench/*/results.json', { import: 'default', eager: true }) as Record<string, unknown>;
const sourceFiles = import.meta.glob(['../../../tools/bench/**/*.R', '../../../tools/bench/typed/*.rs'], { query: '?raw', import: 'default', eager: true }) as Record<string, string>;

export const repository = 'https://github.com/felix-andreas/ry/blob/main';

// The order of the summary, the page and the README.
const suiteOrder = ['public', 'packages', 'kernels', 'applications', 'hot', 'typed'];

export type Interval = { median: number; low?: number; high?: number };
export type Flag = { kind: string; engine?: string; reason?: string };
// An engine on which a program did not run to the end. It has no time or ratio there.
export type Failure = { engine: string; reason: string };
export type TestCounts = { passed: number; failed: number; errors: number };
export type Group = { id: string; name: string; description?: string; url?: string; license?: string };
export type Meta = {
  date: string;
  ryCommit?: string;
  // The release archive measured, when the numbers come from one (`bench.py --release`).
  release?: { tag: string; archive: string; sha256: string };
  // The build of the suite's baseline: GNU R, or the Rust compiler and the flags it ran with.
  gnuR?: { version: string; blas?: string };
  rust?: { version: string; flags: string };
  machine: { cpu: string; cores?: number; kernel?: string };
  rounds: number;
  seed?: number;
  extraRounds?: number;
  tooShortSeconds?: number;
};
export type Program = {
  id: string;
  display: string;
  summary?: string;
  source?: string;
  typedSource?: string;
  rustSource?: string;
  group?: string;
  version?: string;
  // Absent for a program the suite defines but the results file does not hold.
  measurement?: Measurement;
};
export type Measurement = {
  seconds: Record<string, number>;
  ratio: Record<string, Interval>;
  peakMib?: Record<string, number>;
  // What each engine spent compiling the program, where the suite records it.
  compileSeconds?: Record<string, number>;
  // The first run of the program in its process, where the harness times one.
  firstSeconds?: Record<string, number>;
  tests?: Record<string, TestCounts>;
  flags: Flag[];
  failures: Failure[];
};
export type Results = {
  // Engine keys in the order of the results file. `engineLabel` gives each one's name.
  engines: string[];
  // The engine every ratio of the suite compares with.
  baseline: string;
  // A `speedup` is the baseline's time over the engine's, so above 1x the engine is faster. A
  // `slowdown` is the engine's time over the baseline's, so 1x is the baseline's speed and lower
  // is faster.
  ratioKind: RatioKind;
  geomean: Record<string, Interval & { programs: number }>;
  meta: Meta;
};
export type RatioKind = 'speedup' | 'slowdown';
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

// The baseline of a results file that names none is GNU R, and its ratios are speed-ups.
const gnuR = 'gnu_r';

export function baselineOf(suite: Suite): string {
  return suite.results?.baseline ?? gnuR;
}

export function ratioKindOf(suite: Suite): RatioKind {
  return suite.results?.ratioKind ?? 'speedup';
}

// Each results file names its engines, so the labels come from the runner.
const engineLabels = new Map<string, string>();

export function engineLabel(engine: string): string {
  return engineLabels.get(engine) ?? engine;
}

export const suites: Suite[] = loadSuites();

export function suite(id: string): Suite | undefined {
  return suites.find((candidate) => candidate.id === id);
}

// The fragment that links to a suite's section of the benchmarks page, as a Markdown heading of
// the suite's name would get.
export function sectionAnchor(suite: Suite): string {
  return suite.name.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');
}

export function measuredSuites(): Suite[] {
  return suites.filter((candidate) => candidate.results !== undefined);
}

// The measured suites that compare ry with GNU R. Their speed-ups share one scale, so the summary
// table, the landing page and the README chart show them together.
export function gnuRSuites(): Suite[] {
  return measuredSuites().filter((candidate) => baselineOf(candidate) === gnuR);
}

// The engines a suite compares against its baseline, in the order of its results file.
export function comparedEngines(suite: Suite): string[] {
  return (suite.results?.engines ?? []).filter((engine) => engine !== baselineOf(suite));
}

// Typed ry compiled before the program starts (`ry run --aot`). It runs the code that typed ry
// compiles at a first call, so its bar would repeat the typed one: the charts leave it out, and
// the tables show it.
const typedAhead = 'ry_typed_aot';

function chartedEngines(suite: Suite): string[] {
  return comparedEngines(suite).filter((engine) => engine !== typedAhead);
}

// What the time of typed ry compiled ahead holds. A program that times a warm run has no compile
// in it on any engine. A program that times its whole run would leave out the compile that ran
// before it started, so the runner adds that compile to the program's clock.
function aheadNote(programs: (Program & { measurement: Measurement })[]): string {
  const label = engineLabel(typedAhead);
  return programs.some((program) => program.measurement.firstSeconds !== undefined)
    ? `${label} compiles each typed function before the program starts, which the First run table shows.`
    : `${label} compiles each typed function before the program starts. Its time is the program's clock plus that compile, so it counts the same work as the other columns.`;
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

// One unit for all of a suite's times, as in the runner's own table: milliseconds where the suite
// times programs that finish in a few.
export function timeText(suite: Suite): (value: number) => string {
  return tooShortSeconds(suite) < 0.001 ? (value) => `${(value * 1000).toFixed(3)} ms` : seconds;
}

function tooShortSeconds(suite: Suite): number {
  return suite.results?.meta.tooShortSeconds ?? 0.1;
}

export function mebibytes(value: number): string {
  return `${value.toFixed(0)} MiB`;
}

export function failureText(failure: Failure): string {
  return `fails on ${engineLabel(failure.engine)}: ${failure.reason}`;
}

// A flag in words, for the label under a bar and for a table cell.
export function flagText(flag: Flag): string {
  const engine = flag.engine === undefined ? '' : ` on ${engineLabel(flag.engine)}`;
  if (flag.kind === 'too_short') return `too short${engine}`;
  if (flag.kind === 'typed_refused') return `typed code partly refused${flag.reason === undefined ? '' : `: ${flag.reason}`}`;
  if (flag.kind === 'wide_interval') return `wide interval${engine}`;
  if (flag.kind === 'tests_differ') return `tests differ${engine}${flag.reason === undefined ? '' : `: ${flag.reason}`}`;
  return `${flag.kind.replaceAll('_', ' ')}${engine}${flag.reason === undefined ? '' : `: ${flag.reason}`}`;
}

// A ratio rests on the engine's time and on the baseline's, so a flag on either one marks it.
export function ratioFlags(measurement: Measurement, engine: string, baseline: string): Flag[] {
  return measurement.flags.filter((flag) => flag.engine === undefined || flag.engine === engine || flag.engine === baseline);
}

// Whether a program is too short to count: the baseline, whose time measures the work, finished it
// under the floor. A short time on a ry engine is the speed being measured, so it is flagged but counts.
export function isTooShort(measurement: Measurement, baseline: string): boolean {
  return measurement.flags.some((flag) => flag.kind === 'too_short' && (flag.engine === undefined || flag.engine === baseline));
}

export function flagNotes(suite: Suite, programs: Program[]): string[] {
  const baseline = baselineOf(suite);
  const shortest = tooShortSeconds(suite);
  const floor = shortest < 0.001 ? `${Number((shortest * 1000).toPrecision(3))} ms` : `${shortest} s`;
  const flags = programs.flatMap((program) => program.measurement?.flags ?? []);
  const kinds = new Set(flags.map((flag) => flag.kind));
  const notes: string[] = [];
  if (programs.some((program) => program.measurement !== undefined && isTooShort(program.measurement, baseline))) {
    notes.push(`A program marked too short on ${engineLabel(baseline)} finishes there in under ${floor}, which is too little work for a stable ratio, so every geometric mean leaves it out.`);
  }
  if (flags.some((flag) => flag.kind === 'too_short' && flag.engine !== undefined && flag.engine !== baseline)) {
    notes.push(`A time marked too short on a ry engine is under ${floor}. That speed is what the suite measures, so it counts in the geometric mean.`);
  }
  if (kinds.has('typed_refused')) notes.push('A program marked typed code partly refused has code the typed core refused, which runs in the interpreter instead; the flag gives the refusal.');
  if (kinds.has('wide_interval')) notes.push("A program marked wide interval varied so much between rounds that its ratio's interval is wide; read its median with care.");
  if (kinds.has('tests_differ')) notes.push('A package marked tests differ passed a different number of tests on ry than on GNU R; the flag gives both counts.');
  if (programs.some((program) => (program.measurement?.failures.length ?? 0) > 0)) {
    notes.push(`A program marked fails did not run to the end on that engine, or printed another result than ${engineLabel(baseline)}, so it has no ${ratioKindOf(suite) === 'slowdown' ? 'ratio' : 'speed-up'} there and every geometric mean leaves it out; the mark gives the reason.`);
  }
  return notes;
}

// When, where and how a suite was measured, printed under each of its charts and tables.
export function suiteMetadata(suite: Suite): string {
  const results = suite.results;
  if (results === undefined) return '';
  const { meta } = results;
  const build =
    meta.release !== undefined
      ? ` on the ry ${meta.release.tag} release archive (sha256 ${meta.release.sha256.slice(0, 12)})`
      : meta.ryCommit === undefined
        ? ''
        : ` at ry ${shortCommit(meta.ryCommit)}`;
  const against =
    meta.rust !== undefined
      ? `against Rust ${meta.rust.version}, compiled with rustc ${meta.rust.flags},`
      : `with GNU R ${meta.gnuR?.version ?? 'of an unrecorded version'}`;
  return `Measured on ${meta.date.slice(0, 10)}${build} ${against} on ${machine(meta)}; ${meta.rounds} paired rounds.`;
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

// The headline of one measured suite that compares ry with GNU R: ry's geometric-mean speed-up,
// untyped and, where the suite has a typed column, typed in strict mode. tools/bench/readme.py writes the same numbers into
// the READMEs, and the landing page draws them as bars.
export type SuiteSpeed = {
  id: string;
  name: string;
  // The name without the `(ours)` marker, for places where the summary already says "we wrote".
  shortName: string;
  programs: number;
  summary: string;
  ry?: Interval & { programs: number };
  typed?: Interval & { programs: number };
};

export function suiteSpeeds(): SuiteSpeed[] {
  return gnuRSuites().map((suite) => {
    const geomean = suite.results?.geomean ?? {};
    return {
      id: suite.id,
      name: suite.name,
      shortName: suite.name.replace(/\s*\(ours\)/, ''),
      programs: measuredPrograms(suite).length,
      summary: suite.summary,
      ry: geomean.ry,
      typed: geomean.ry_typed,
    };
  });
}

// Whether the suites ran on different machines (CPU model or kernel) or against a GNU R on a
// different BLAS, which makes their ratios incomparable with each other.
export function mixedMachines(): boolean {
  const machines = new Set(
    gnuRSuites().map((suite) => {
      const meta = suite.results?.meta;
      return JSON.stringify([meta?.machine.cpu, meta?.machine.kernel, meta?.gnuR?.blas]);
    }),
  );
  return machines.size > 1;
}

// One sentence per measured suite whose ratios are slowdowns, which the summary table of speed-ups
// cannot hold: the suite's geometric mean and how to read it.
export function slowdownSummaries(): string[] {
  return measuredSuites()
    .filter((suite) => ratioKindOf(suite) === 'slowdown')
    .flatMap((suite) => {
      const engine = comparedEngines(suite)[0];
      const mean = suite.results?.geomean[engine];
      if (mean === undefined) return [];
      const baseline = engineLabel(baselineOf(suite));
      return [`${suite.name}: ${engineLabel(engine)} takes ${formatRatio(mean.median)} ${baseline}'s time, the geometric mean over ${mean.programs} ${suite.summary}. Here 1x is ${baseline}'s speed, and lower is faster.`];
    });
}

// One row per measured suite that compares ry with GNU R: what it is and ry's geometric-mean speed
// against GNU R, untyped and typed in strict mode.
export const summaryColumns = ['Suite', 'What it is', 'ry vs GNU R', 'ry typed'];

export function summaryRows(): string[][] {
  return suiteSpeeds().map((speed) => [
    speed.name,
    `${speed.programs} ${speed.summary}`,
    speed.ry === undefined ? 'not measured' : formatRatio(speed.ry.median),
    speed.typed === undefined ? 'not typed' : formatRatio(speed.typed.median),
  ]);
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
    ...slowdownSummaries().flatMap((sentence) => [sentence, '']),
    ...measuredSuites().map((suite) => `- ${suite.name}: ${lowercaseFirst(suiteMetadata(suite))}`),
  ].join('\n');
}

// A bar chart of a suite's ratios to its baseline: one row per program, one bar per engine, on a
// logarithmic axis centred on 1x, so 10x faster and 10x slower are bars of the same length.
// `measure` names what a bar shows, and `axisNote` says which side of 1x is faster.
export type ChartBar = { engine: string; ratio?: Interval; faded: boolean; title: string };
export type ChartRow = { label: string; id?: string; href?: string; flags: string[]; bars: ChartBar[]; summary?: boolean };
export type Chart = { caption: string; measure: string; axisNote: string; engines: string[]; rows: ChartRow[]; notes: string[]; metadata: string };

// One chart per group, or one for the whole suite when it has no groups.
export function suiteCharts(suite: Suite): Chart[] {
  const engines = chartedEngines(suite);
  const programs = measuredPrograms(suite);
  const groups = suite.groups.length === 0 ? [undefined] : suite.groups;
  const baseline = engineLabel(baselineOf(suite));
  const slowdown = ratioKindOf(suite) === 'slowdown';
  // The fastest program comes first either way, and a program without a ratio last.
  const rank = (row: ChartRow) => {
    const median = row.bars[0]?.ratio?.median;
    return median === undefined ? Infinity : slowdown ? median : -median;
  };
  const charts: Chart[] = groups.flatMap((group) => {
    const members = group === undefined ? programs : programs.filter((program) => program.group === group.id);
    if (members.length === 0) return [];
    const rows: ChartRow[] = members.map((program) => chartRow(suite, program, engines)).sort((a, b) => rank(a) - rank(b));
    return [{
      caption: group === undefined ? suite.name : `${suite.name}: ${group.name}`,
      measure: slowdown ? `time relative to ${baseline}'s` : `speed-up over ${baseline}`,
      axisNote: slowdown
        ? `Time relative to ${baseline}'s on a log scale: 1x is ${baseline}'s speed, a bar to the right of it is slower than ${baseline}, to the left faster.`
        : `Speed relative to ${baseline} on a log scale: a bar to the right of 1x is faster than ${baseline}, to the left slower.`,
      engines,
      rows,
      notes: flagNotes(suite, members),
      metadata: suiteMetadata(suite),
    }];
  });
  // The geometric mean spans the whole suite, so it closes the last chart.
  charts.at(-1)?.rows.push(geomeanRow(suite, engines, groups.length > 1));
  return charts;
}

function chartRow(suite: Suite, program: Program & { measurement: Measurement }, engines: string[]): ChartRow {
  const { measurement } = program;
  const baseline = baselineOf(suite);
  const baselineTime = measurement.seconds[baseline];
  const time = timeText(suite);
  const source = program.source ?? program.typedSource;
  return {
    label: program.display,
    id: program.id === program.display ? undefined : program.id,
    href: source === undefined ? undefined : sourceUrl(source),
    flags: [...measurement.failures.map(failureText), ...measurement.flags.map(flagText)],
    bars: engines.map((engine) => {
      const ratio = measurement.ratio[engine];
      const engineTime = measurement.seconds[engine];
      const failure = measurement.failures.find((candidate) => candidate.engine === engine || candidate.engine === baseline);
      const title = ratio === undefined
        ? `${program.display}, ${engineLabel(engine)}: ${failure === undefined ? 'not measured' : failureText(failure)}`
        : `${program.display}, ${engineLabel(engine)}: ${formatInterval(ratio)} the ${ratioKindOf(suite) === 'slowdown' ? 'time' : 'speed'} of ${engineLabel(baseline)}` +
          (engineTime === undefined || baselineTime === undefined ? '' : `, ${time(engineTime)} against ${time(baselineTime)}`);
      return { engine, ratio, faded: isTooShort(measurement, baseline), title };
    }),
  };
}

function geomeanRow(suite: Suite, engines: string[], grouped: boolean): ChartRow {
  const geomean = suite.results?.geomean ?? {};
  // A failing or too-short program leaves the mean, so the label says how many it covers.
  const counted = geomean[engines[0]]?.programs;
  const total = measuredPrograms(suite).length;
  return {
    label: `${grouped ? 'geometric mean of all groups' : 'geometric mean'}${counted !== undefined && counted < total ? `, ${counted} of ${total} programs` : ''}`,
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
        `${chart.caption}, ${chart.measure}:`,
        '',
        `| program | ${chart.engines.map(engineLabel).join(' | ')} |`,
        `| --- |${chart.engines.map(() => ' ---: |').join('')}`,
        ...chart.rows.map((row) => {
          const label = `${row.label}${row.id === undefined ? '' : ` (\`${row.id}\`)`}${row.flags.length === 0 ? '' : `, ${row.flags.join(', ')}`}`;
          return `| ${label} | ${row.bars.map((bar) => (bar.ratio === undefined ? 'n/a' : formatInterval(bar.ratio))).join(' | ')} |`;
        }),
        '',
        [chart.axisNote, ...chart.notes, chart.metadata].join(' '),
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
  const baseline = baselineOf(suite);
  const slowdown = ratioKindOf(suite) === 'slowdown';
  const time = timeText(suite);
  const name = (program: Program) => `${program.display}${program.version === undefined ? '' : ` ${program.version}`}`;
  const views: TableView[] = [{
    label: 'Time',
    columns: [
      'program',
      ...engines.map(engineLabel),
      ...compared.map((engine) => (slowdown ? `${engineLabel(engine)} / ${engineLabel(baseline)}` : `${engineLabel(engine)}, speed-up`)),
    ],
    rows: programs.map((program) => [
      name(program),
      ...engines.map((engine) => {
        const value = program.measurement.seconds[engine];
        return value === undefined ? 'n/a' : time(value);
      }),
      ...compared.map((engine) => {
        const ratio = program.measurement.ratio[engine];
        const flags = [
          ...program.measurement.failures.filter((failure) => failure.engine === engine || failure.engine === baseline).map(failureText),
          ...ratioFlags(program.measurement, engine, baseline).map(flagText),
        ];
        return `${ratio === undefined ? 'n/a' : formatInterval(ratio)}${flags.length === 0 ? '' : ` (${flags.join(', ')})`}`;
      }),
    ]),
    note: (slowdown
      ? `Each time is the median over the rounds. Each ratio is the median of the per-round ratios of the engine's time over ${engineLabel(baseline)}'s, with its 95% interval in parentheses.`
      : 'Each time is the median over the rounds. Each speed-up is the median of the per-round ratios, with its 95% interval in parentheses.')
      + (engines.includes(typedAhead) ? ` ${aheadNote(programs)}` : ''),
  }];
  const first = programs.filter((program) => program.measurement.firstSeconds !== undefined);
  if (first.length > 0) {
    views.push({
      label: 'First run',
      columns: ['program', ...engines.map(engineLabel)],
      rows: first.map((program) => [
        name(program),
        ...engines.map((engine) => {
          const value = program.measurement.firstSeconds?.[engine];
          return value === undefined ? 'n/a' : time(value);
        }),
      ]),
      note: 'The first run of the program in its process, as the median over the rounds. It holds whatever compiles at a first call, which the warm runs of the Time table do not.'
        + (engines.includes(typedAhead) ? ` ${engineLabel(typedAhead)} compiled the program before it started, so its first run compiles nothing.` : ''),
    });
  }
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
  const compiled = programs.filter((program) => program.measurement.compileSeconds !== undefined);
  if (compiled.length > 0) {
    const milliseconds = (value: number | undefined) => (value === undefined ? 'n/a' : `${(value * 1000).toFixed(1)} ms`);
    views.push({
      label: 'Compile time',
      columns: ['program', ...engines.map(engineLabel)],
      rows: [
        ...compiled.map((program) => [name(program), ...engines.map((engine) => milliseconds(program.measurement.compileSeconds?.[engine]))]),
        ['total', ...engines.map((engine) => milliseconds(compiled.reduce((sum, program) => sum + (program.measurement.compileSeconds?.[engine] ?? 0), 0)))],
      ],
      note: `Compile time is outside every timed run. ${engineLabel(baseline)}'s is one rustc process per program. The typed core's is the sum over the functions it compiled, as RY_TYPED_LOG reports them.`,
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
  const start = lines.findIndex((line) => !line.startsWith('#') && !line.startsWith('//'));
  return lines.slice(Math.max(start, 0)).join('\n').trim();
}

// The versions of a program a reader can open, in the order the page shows them.
export type ProgramVersion = { label: string; path: string; language: string; code?: string };

export function programVersions(program: Program): ProgramVersion[] {
  const versions: [string, string | undefined, string][] = [
    ['Plain R', program.source, 'r'],
    ['Typed', program.typedSource, 'r'],
    ['Rust', program.rustSource, 'rust'],
  ];
  return versions.flatMap(([label, path, language]) => (path === undefined ? [] : [{ label, path, language, code: programCode(path) }]));
}

// A program's headline number: the ratio of the first engine its suite compares with the
// baseline, which is ry's speed-up on the untyped program, or typed ry's time over Rust's.
export function programRatio(suite: Suite, program: Program): string {
  const measurement = program.measurement;
  if (measurement === undefined) return 'not in the committed results';
  const baseline = baselineOf(suite);
  const failure = measurement.failures[0];
  if (failure !== undefined) return failure.engine === baseline ? `fails on ${engineLabel(baseline)} too` : `fails on ${engineLabel(failure.engine)}`;
  const ratio = measurement.ratio[comparedEngines(suite)[0]];
  if (ratio === undefined) return '';
  const reading = ratioKindOf(suite) === 'slowdown' ? ` ${engineLabel(baseline)}'s time` : '';
  return `${formatRatio(ratio.median)}${reading}${isTooShort(measurement, baseline) ? ' (too short)' : ''}`;
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
        const versions = programVersions(program);
        const lines = [`- **${program.display}** ([\`${program.id}\`](${sourceUrl(versions[0]?.path ?? suite.directory)}), ${programRatio(suite, program)}): ${program.summary ?? ''}`];
        if (versions.length > 1 && versions.every((version) => version.code !== undefined)) {
          for (const version of versions) {
            lines.push('', `  ${version.label}:`, '', `  \`\`\`${version.language}`, ...(version.code ?? '').split('\n').map((line) => `  ${line}`), '  ```');
          }
        }
        return lines.join('\n');
      });
      return [...intro, ...entries].join('\n');
    })
    .join('\n\n');
}

// How each measured suite was run, one row per suite. The date, machine and commit are left out
// because the line under each of the suite's charts and tables gives them.
export function methodRows(): { columns: string[]; rows: string[][] } {
  return {
    columns: ['Suite', 'Reproduce', 'Baseline', 'Engines', 'Samples'],
    rows: measuredSuites().map((suite) => {
      const meta = suite.results?.meta;
      const blas = meta?.gnuR?.blas?.split('/').pop();
      return [
        suite.name,
        `\`just bench ${suite.id} --release ARCHIVE --write\``,
        meta?.rust !== undefined
          ? `Rust ${meta.rust.version}, \`rustc ${meta.rust.flags}\``
          : `GNU R ${meta?.gnuR?.version ?? ''}${blas === undefined ? '' : `, BLAS \`${blas}\``}`,
        (suite.results?.engines ?? []).map(engineLabel).join(', '),
        `${meta?.rounds} paired rounds${meta?.extraRounds ? `, ${meta.extraRounds} more for each wide interval` : ''}${meta?.seed === undefined ? '' : `, seed ${meta.seed}`}`,
      ];
    }),
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
  programs?: { id: string; display: string; summary?: string; source?: string; typed_source?: string; rust_source?: string; group?: string; version?: string }[];
};

type RawResults = {
  schema: number;
  engines: Record<string, string>;
  baseline?: string;
  ratio_kind?: RatioKind;
  meta: {
    date: string;
    ry_commit?: string | null;
    release?: { tag: string; archive: string; sha256: string } | null;
    gnu_r?: { version: string; blas?: string | null } | null;
    rust?: { version: string; flags: string } | null;
    machine: { cpu: string; cores?: number | null; kernel?: string | null };
    rounds: number;
    extra_rounds?: number | null;
    seed?: number | null;
    too_short_seconds?: number | null;
  };
  programs: {
    id: string;
    display?: string;
    seconds: Record<string, number>;
    ratio: Record<string, Interval>;
    peak_mib?: Record<string, number>;
    compile_seconds?: Record<string, number>;
    first_seconds?: Record<string, number>;
    tests?: Record<string, TestCounts>;
    flags?: Flag[];
    failures?: Failure[];
  }[];
  geomean: Record<string, Interval & { programs: number }>;
};

function loadSuites(): Suite[] {
  const loaded = Object.entries(manifests).map(([path, text]) => {
    const directory = path.replace(/^\.\.\/\.\.\/\.\.\//, '').replace(/\/suite\.toml$/, '');
    const manifest = parse(text) as unknown as Manifest;
    const raw = resultFiles[`../../../${directory}/results.json`];
    const results = raw === undefined ? undefined : checkSchema(manifest.id, raw);
    for (const [key, label] of Object.entries(results?.engines ?? {})) engineLabels.set(key, label);
    const byId = new Map((results?.programs ?? []).map((program) => [program.id, program]));
    const programs: Program[] = (manifest.programs ?? []).map((program) => ({
      id: program.id,
      display: program.display,
      summary: program.summary,
      source: program.source,
      typedSource: program.typed_source,
      rustSource: program.rust_source,
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
        : {
            engines: Object.keys(results.engines),
            baseline: results.baseline ?? gnuR,
            ratioKind: results.ratio_kind ?? 'speedup',
            geomean: results.geomean,
            meta: meta(results.meta),
          },
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
  return { seconds: program.seconds, ratio: program.ratio, peakMib: program.peak_mib, compileSeconds: program.compile_seconds, firstSeconds: program.first_seconds, tests: program.tests, flags: program.flags ?? [], failures: program.failures ?? [] };
}

function meta(raw: RawResults['meta']): Meta {
  return {
    // A runner written in Python stores an absent value as null, which reads here as absent.
    date: raw.date,
    ryCommit: raw.ry_commit ?? undefined,
    release: raw.release ?? undefined,
    gnuR: raw.gnu_r == null ? undefined : { version: raw.gnu_r.version, blas: raw.gnu_r.blas ?? undefined },
    rust: raw.rust ?? undefined,
    machine: { cpu: raw.machine.cpu, cores: raw.machine.cores ?? undefined, kernel: raw.machine.kernel ?? undefined },
    rounds: raw.rounds,
    seed: raw.seed ?? undefined,
    extraRounds: raw.extra_rounds ?? undefined,
    tooShortSeconds: raw.too_short_seconds ?? undefined,
  };
}

function checkSchema(id: string, raw: unknown): RawResults {
  const value = raw as { schema?: number };
  if (value.schema !== 1) throw new Error(`benchmark suite \`${id}\`: results.json has schema ${value.schema}, which the docs do not read`);
  return raw as RawResults;
}
