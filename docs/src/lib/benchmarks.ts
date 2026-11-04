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
  // Peak resident memory of the whole process in MiB, R's startup included.
  peak: { gnuR: number; ry: number; ryTyped: number };
};
export type BenchmarkProgram = { description: string; untyped: string; typed: string };
// One view of the measurement, shown as one tab of every benchmark table.
export type BenchmarkView = { label: string; columns: string[]; cells: (row: BenchmarkRow) => string[]; note?: string };

export const benchmarkMachine = `${results.machine}, against GNU R ${/\d+\.\d+\.\d+/.exec(results.r)?.[0] ?? results.r}`;

const engines = ['GNU R', 'ry', 'ry, typed and strict'];

// Time with ry's speed-up over GNU R, and peak memory with ry's ratio to GNU R, where above 1x means
// ry uses more.
export const benchmarkViews: BenchmarkView[] = [
  {
    label: 'CPU',
    columns: ['program', ...engines, 'speed-up'],
    cells: (row) => [row.program, seconds(row.gnuR), seconds(row.ry), seconds(row.ryTyped), `${row.speedup.toFixed(1)}x`],
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
  return parts.join('\n\n');
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

function seconds(value: number): string {
  return `${value.toFixed(3)} s`;
}

function mebibytes(value: number): string {
  return `${value.toFixed(0)} MiB`;
}

export type RealWorkloadRow = { workload: string; gnuR: number; ryJit: number; ryNoJit: number };

// The call-heavy workloads of `tools/bench/real/results.json`, which `just bench-real --write` regenerates.
export function realWorkloadRows(): RealWorkloadRow[] {
  return realResults.workloads.map((row) => ({
    workload: row.workload,
    gnuR: row.gnu_r,
    ryJit: row.ry_jit,
    ryNoJit: row.ry_nojit,
  }));
}

// GNU R's time over ry's, as a geometric mean: above 1x means ry is faster.
export function geomean(ratios: number[]): number {
  return Math.exp(ratios.reduce((sum, ratio) => sum + Math.log(ratio), 0) / ratios.length);
}

export const kernelGeomean = geomean(results.programs.map((row) => row.gnu_r / row.ry));
export const kernelCount = results.programs.length;
export const realGeomean = geomean(realWorkloadRows().map((row) => row.gnuR / row.ryJit));
export const realGeomeanNoJit = geomean(realWorkloadRows().map((row) => row.gnuR / row.ryNoJit));
export const realCount = realWorkloadRows().length;

export const benchmarkRealMachine = `${realResults.machine}, against GNU R ${/\d+\.\d+\.\d+/.exec(realResults.r)?.[0] ?? realResults.r}`;

// What each of the two benchmarks measures and how ry fares on it, for the side-by-side summary.
export function benchmarkSummaryRows(): string[][] {
  return [
    ['Real-world workloads', 'call-heavy R: closures, S3 and S4 dispatch, conditions, environments, strings, lists', `${realCount}`, `${realGeomean.toFixed(2)}x`],
    ['Synthetic kernels', 'tight loops, recursion and vector kernels, where the JIT applies', `${kernelCount}`, `${kernelGeomean.toFixed(2)}x`],
  ];
}
export const benchmarkSummaryColumns = ['untyped R on ry', 'what it measures', 'programs', 'speed vs GNU R (geometric mean)'];

export function realWorkloadMarkdown(): string {
  const rows = realWorkloadRows();
  return [
    '| workload | GNU R | ry, JIT on | ry, JIT off | GNU R / ry, JIT on | GNU R / ry, JIT off |',
    '| --- | ---: | ---: | ---: | ---: | ---: |',
    ...rows.map(
      (row) =>
        `| \`${row.workload}\` | ${seconds(row.gnuR)} | ${seconds(row.ryJit)} | ${seconds(row.ryNoJit)} | ${(row.gnuR / row.ryJit).toFixed(2)}x | ${(row.gnuR / row.ryNoJit).toFixed(2)}x |`,
    ),
    `| geometric mean | | | | ${realGeomean.toFixed(2)}x | ${realGeomeanNoJit.toFixed(2)}x |`,
  ].join('\n');
}

export function benchmarkSummaryMarkdown(): string {
  return [
    `| ${benchmarkSummaryColumns.join(' | ')} |`,
    '| --- | --- | ---: | ---: |',
    ...benchmarkSummaryRows().map((row) => `| ${row.join(' | ')} |`),
  ].join('\n');
}
