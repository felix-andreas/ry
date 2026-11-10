// The benchmark the docs show: `tools/bench/why/results.json`, which `just bench-why --write`
// regenerates, and the programs it measured. Pages render both from here, so a table never drifts
// from the measurement or from the code it measured.
import results from '../../../tools/bench/why/results.json';
import realResults from '../../../tools/bench/real/results.json';

const sources = import.meta.glob('../../../tools/bench/why/{untyped,typed}/*.R', {
  query: '?raw',
  import: 'default',
  eager: true,
}) as Record<string, string>;

export type BenchmarkRow = {
  program: string;
  gnuR: number;
  ry: number;
  ryTyped: number;
  speedup: number;
  // GNU R finishes in under `TOO_SHORT_SECONDS`, so the speed-up mostly measures the timer.
  tooShort: boolean;
  // Peak resident memory of the whole process in MiB, R's startup included.
  peak: { gnuR: number; ry: number; ryTyped: number };
};
export type BenchmarkProgram = { description: string; untyped: string; typed: string };
// One view of the measurement, shown as one tab of every benchmark table.
export type BenchmarkView = { label: string; columns: string[]; cells: (row: BenchmarkRow) => string[]; note?: string };

// The same floor `tools/bench/real/run.py` applies: a row this short is marked and left out of
// every geometric mean.
export const TOO_SHORT_SECONDS = 0.1;

export const benchmarkMachine = `${results.machine}, against GNU R ${rVersion(results.r)}`;

const engines = ['GNU R', 'ry', 'ry, typed and strict'];

// Time with ry's speed-up over GNU R, and peak memory with ry's ratio to GNU R, where above 1x means
// ry uses more.
export const benchmarkViews: BenchmarkView[] = [
  {
    label: 'CPU',
    columns: ['program', ...engines, 'speed-up'],
    cells: (row) => [row.program, seconds(row.gnuR), seconds(row.ry), seconds(row.ryTyped), `${formatRatio(row.speedup)}${row.tooShort ? ' (too short)' : ''}`],
  },
  {
    label: 'Memory',
    columns: ['program', ...engines, 'ry / GNU R'],
    cells: (row) => [
      row.program,
      mebibytes(row.peak.gnuR),
      mebibytes(row.peak.ry),
      mebibytes(row.peak.ryTyped),
      `${(row.peak.ry / row.peak.gnuR).toFixed(1)}x`,
    ],
    note: "Peak memory is the peak resident set size of the whole process, R's startup included.",
  },
];

// The rows in the results' order, or only the named programs in the order given.
export function benchmarkRows(programs?: string[]): BenchmarkRow[] {
  const rows = results.programs.map((row) => ({
    program: row.program,
    gnuR: row.gnu_r,
    ry: row.ry,
    ryTyped: row.ry_typed,
    speedup: row.gnu_r / row.ry,
    tooShort: row.gnu_r < TOO_SHORT_SECONDS,
    peak: { gnuR: row.peak_mib.gnu_r, ry: row.peak_mib.ry, ryTyped: row.peak_mib.ry_typed },
  }));
  if (programs === undefined) return rows;
  return programs.map((name) => {
    const row = rows.find((candidate) => candidate.program === name);
    if (row === undefined) throw new Error(`benchmark program \`${name}\` is not in results.json`);
    return row;
  });
}

// A program's leading comment describes it, so it becomes the prose and the code starts after it.
export function benchmarkProgram(program: string): BenchmarkProgram {
  const untyped = source('untyped', program);
  const lines = untyped.split('\n');
  const commentLines = lines.findIndex((line) => !line.startsWith('#'));
  return {
    description: lines.slice(0, commentLines).map((line) => line.replace(/^#\s?/, '')).join(' '),
    untyped: lines.slice(commentLines).join('\n').trim(),
    typed: source('typed', program).split('\n').slice(commentLines).join('\n').trim(),
  };
}

// Each view as a table under its own heading, the way a page's tabs become subheadings.
export function benchmarkTableMarkdown(headingLevel: number, programs?: string[]): string {
  const rows = benchmarkRows(programs);
  const parts = benchmarkViews.map((view) =>
    [
      `${'#'.repeat(Math.min(headingLevel, 6))} ${view.label}`,
      '',
      `| ${view.columns.join(' | ')} |`,
      `| --- | ${view.columns.slice(1).map(() => '---:').join(' | ')} |`,
      ...rows.map((row) => `| \`${row.program}\` | ${view.cells(row).slice(1).join(' | ')} |`),
      ...(view.note === undefined ? [] : ['', view.note]),
    ].join('\n'),
  );
  return [...parts, suiteMetadata('kernels')].join('\n\n');
}

export function benchmarkProgramsMarkdown(headingLevel: number): string {
  const heading = '#'.repeat(Math.min(headingLevel, 6));
  const parts = [...benchmarkRows().map((row) => row.program), 'harness'].map((name) => {
    const program = benchmarkProgram(name);
    return [
      `${heading} ${name}`,
      '',
      program.description,
      '',
      'Plain R:',
      '',
      '```r',
      program.untyped,
      '```',
      '',
      'Typed:',
      '',
      '```r',
      program.typed,
      '```',
    ].join('\n');
  });
  return parts.join('\n\n');
}

function source(variant: 'untyped' | 'typed', program: string): string {
  const text = sources[`../../../tools/bench/why/${variant}/${program}.R`];
  if (text === undefined) throw new Error(`no benchmark source tools/bench/why/${variant}/${program}.R`);
  return text;
}

export function seconds(value: number): string {
  return `${value.toFixed(3)}\u00a0s`;
}

function mebibytes(value: number): string {
  return `${value.toFixed(0)}\u00a0MiB`;
}

export type RealWorkloadRow = { workload: string; gnuR: number; ryJit: number; ryNoJit: number; tooShort: boolean };

// The call-heavy workloads of `tools/bench/real/results.json`, which `just bench-real --write` regenerates.
export function realWorkloadRows(): RealWorkloadRow[] {
  return realResults.workloads.map((row) => ({
    workload: row.workload,
    gnuR: row.gnu_r,
    ryJit: row.ry_jit,
    ryNoJit: row.ry_nojit,
    tooShort: Math.min(row.gnu_r, row.ry_jit, row.ry_nojit) < TOO_SHORT_SECONDS,
  }));
}

// GNU R's time over ry's, as a geometric mean: above 1x means ry is faster.
export function geomean(ratios: number[]): number {
  return Math.exp(ratios.reduce((sum, ratio) => sum + Math.log(ratio), 0) / ratios.length);
}

const kernelRows = benchmarkRows().filter((row) => !row.tooShort);
export const kernelGeomean = geomean(kernelRows.map((row) => row.gnuR / row.ry));
export const kernelTypedGeomean = geomean(kernelRows.map((row) => row.gnuR / row.ryTyped));
export const kernelCount = kernelRows.length;
const realRows = realWorkloadRows().filter((row) => !row.tooShort);
export const realGeomean = geomean(realRows.map((row) => row.gnuR / row.ryJit));
export const realGeomeanNoJit = geomean(realRows.map((row) => row.gnuR / row.ryNoJit));
export const realCount = realRows.length;

export const benchmarkRealMachine = `${realResults.machine}, against GNU R ${rVersion(realResults.r)}`;

// When, where and how often a suite was measured, printed under each of its charts and tables.
export function suiteMetadata(suite: 'real' | 'kernels'): string {
  const source = suite === 'real' ? realResults : results;
  const commit = (source as { commit?: string }).commit;
  const samples = suite === 'real' ? `${source.repeat} rounds` : `the best of ${source.repeat} processes per engine`;
  return `Measured on ${source.date}${commit === undefined ? '' : ` at ry ${commit}`} with GNU R ${rVersion(source.r)} on ${source.machine}; ${samples}.`;
}

// What each of the two benchmarks measures and how ry fares on it, for the side-by-side summary.
export function benchmarkSummaryRows(): string[][] {
  return [
    ['Application-style workloads (ours)', 'call-heavy R: closures, S3 and S4 dispatch, conditions', `${realCount}`, formatRatio(realGeomean)],
    ['Synthetic kernels (ours)', 'loops, recursion and vector code', `${kernelCount}`, formatRatio(kernelGeomean)],
  ];
}
export const benchmarkSummaryColumns = ['benchmark', 'what it measures', 'programs in the mean', 'speed-up over GNU R'];

export function realWorkloadMarkdown(): string {
  const rows = realWorkloadRows();
  return [
    '| workload | GNU R | ry, JIT on | ry, JIT off | speed-up, JIT on | speed-up, JIT off |',
    '| --- | ---: | ---: | ---: | ---: | ---: |',
    ...rows.map(
      (row) =>
        `| \`${row.workload}\`${row.tooShort ? ' (too short)' : ''} | ${seconds(row.gnuR)} | ${seconds(row.ryJit)} | ${seconds(row.ryNoJit)} | ${formatRatio(row.gnuR / row.ryJit)} | ${formatRatio(row.gnuR / row.ryNoJit)} |`,
    ),
    `| geometric mean | | | | ${formatRatio(realGeomean)} | ${formatRatio(realGeomeanNoJit)} |`,
    '',
    suiteMetadata('real'),
  ].join('\n');
}

export function benchmarkSummaryMarkdown(): string {
  return [
    `| ${benchmarkSummaryColumns.join(' | ')} |`,
    '| --- | --- | ---: | ---: |',
    ...benchmarkSummaryRows().map((row) => `| ${row.join(' | ')} |`),
  ].join('\n');
}

// A bar chart of speed relative to GNU R: one row per program, one bar per ry configuration, on a
// logarithmic axis centred on 1x, so 10x faster and 10x slower are bars of the same length.
export type SpeedSeries = { label: string };
export type SpeedRow = { label: string; ratios: number[]; details: string[]; summary?: boolean; tooShort?: boolean };
export type SpeedChart = { series: SpeedSeries[]; rows: SpeedRow[]; metadata: string };

export function kernelChart(): SpeedChart {
  const rows = benchmarkRows()
    .sort((a, b) => b.speedup - a.speedup)
    .map((row) => ({
      label: row.program,
      ratios: [row.gnuR / row.ry, row.gnuR / row.ryTyped],
      tooShort: row.tooShort,
      details: [
        `${row.program}, ry: ${seconds(row.ry)} against ${seconds(row.gnuR)} on GNU R`,
        `${row.program}, ry typed and strict: ${seconds(row.ryTyped)} against ${seconds(row.gnuR)} on GNU R`,
      ],
    }));
  rows.push({
    label: 'geometric mean',
    ratios: [kernelGeomean, kernelTypedGeomean],
    details: ['geometric mean over all programs, ry', 'geometric mean over all programs, ry typed and strict'],
    summary: true,
  });
  return { series: [{ label: 'ry' }, { label: 'ry, typed and strict' }], rows, metadata: suiteMetadata('kernels') };
}

export function realChart(): SpeedChart {
  const rows: SpeedRow[] = realWorkloadRows()
    .sort((a, b) => b.gnuR / b.ryJit - a.gnuR / a.ryJit)
    .map((row) => ({
      label: row.workload,
      ratios: [row.gnuR / row.ryJit],
      tooShort: row.tooShort,
      details: [`${row.workload}: ${seconds(row.ryJit)} on ry against ${seconds(row.gnuR)} on GNU R`],
    }));
  rows.push({ label: 'geometric mean', ratios: [realGeomean], details: ['geometric mean over all workloads'], summary: true });
  return { series: [{ label: 'ry' }], rows, metadata: suiteMetadata('real') };
}

// The Markdown export has no bar chart, so it lists the same ratios as a table.
export function speedChartMarkdown(name: 'real' | 'kernels'): string {
  const chart = name === 'real' ? realChart() : kernelChart();
  return [
    `| ${['program', ...chart.series.map((series) => `${series.label}, speed-up over GNU R`)].join(' | ')} |`,
    `| --- |${chart.series.map(() => ' ---: |').join('')}`,
    ...chart.rows.map(
      (row) => `| ${row.summary ? row.label : `\`${row.label}\`${row.tooShort ? ' (too short)' : ''}`} | ${row.ratios.map(formatRatio).join(' | ')} |`,
    ),
    '',
    chart.metadata + (chart.rows.some((row) => row.tooShort) ? ` ${tooShortNote(name)}` : ''),
  ].join('\n');
}

// Whole numbers from 10x up, where a decimal adds nothing, and two decimals below, where 0.94x
// against 1.02x is the point.
export function formatRatio(ratio: number): string {
  return ratio >= 10 ? `${ratio.toFixed(0)}x` : `${ratio.toFixed(2)}x`;
}

export function tooShortNote(suite: 'real' | 'kernels'): string {
  const where = suite === 'real' ? 'on at least one engine' : 'on GNU R';
  return `A program marked too short takes under ${TOO_SHORT_SECONDS} s ${where}, so its speed-up mostly measures the timer, and the geometric mean leaves it out.`;
}

function rVersion(text: string): string {
  return /\d+\.\d+\.\d+/.exec(text)?.[0] ?? text;
}

export const benchmarkRounds = realResults.repeat;
export const kernelRepeat = results.repeat;

export function benchmarkFact(suite: 'real' | 'kernels', field: 'machine' | 'repeat'): string {
  if (field === 'machine') return suite === 'real' ? benchmarkRealMachine : benchmarkMachine;
  return suite === 'real'
    ? `${realResults.repeat} rounds, each running every engine once in a shuffled order; each engine's median time is reported`
    : `${results.repeat} processes per engine, the engines one after another for each program; the fastest time and the largest memory peak are reported`;
}
