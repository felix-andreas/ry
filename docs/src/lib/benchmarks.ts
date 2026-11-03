// The benchmark the docs show: `tools/bench/why/results.json`, which `just bench-why --write`
// regenerates, and the programs it measured. Pages render both from here, so a table never drifts
// from the measurement or from the code it measured.
import results from '../../../tools/bench/why/results.json';

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

export const benchmarkMachine = `${results.machine}, against GNU R ${/\d+\.\d+\.\d+/.exec(results.r)?.[0] ?? results.r}`;
export const benchmarkEngines = ['GNU R, untyped', 'ry, untyped', 'ry, typed and strict'];
export const benchmarkColumns = [
  'program',
  ...benchmarkEngines,
  'ry vs GNU R',
  ...benchmarkEngines.map((engine) => `${engine}, peak memory`),
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

export function benchmarkCells(row: BenchmarkRow): string[] {
  return [
    row.program,
    seconds(row.gnuR),
    seconds(row.ry),
    seconds(row.ryTyped),
    `${row.speedup.toFixed(1)}x`,
    mebibytes(row.peak.gnuR),
    mebibytes(row.peak.ry),
    mebibytes(row.peak.ryTyped),
  ];
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

export function benchmarkTableMarkdown(programs?: string[]): string {
  const lines = [
    `| ${benchmarkColumns.join(' | ')} |`,
    `| --- | ${benchmarkColumns.slice(1).map(() => '---:').join(' | ')} |`,
    ...benchmarkRows(programs).map((row) => `| ${benchmarkCells(row).join(' | ')} |`),
  ];
  return lines.join('\n');
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
