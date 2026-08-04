/* Charts for the report page. Series data is embedded by src/command/report.rs as report-data. */

const DATA = JSON.parse(document.getElementById('report-data').textContent);

const token = (name) => getComputedStyle(document.documentElement).getPropertyValue(name).trim();

const THEME = {
  surface: token('--surface'),
  elevated: token('--elevated'),
  foreground: token('--foreground'),
  muted: token('--muted'),
  faint: token('--faint'),
  border: token('--border'),
  primary: token('--primary'),
};

/* Slot orders, not 1..8: adjacent pairs have to stay apart under colour-blind simulation, and the
   two charts start on different hues so their first series never share a colour. */
const KIND_COLORS = [1, 6, 2, 7, 5, 3, 8, 4].map((step) => token('--cat-' + step));
const LINE_COLORS = [6, 1, 7, 2].map((step) => token('--cat-' + step));

const at = (palette, index) => palette[index % palette.length];

const FONT = "'Space Grotesk', system-ui, sans-serif";

const TOTAL_SERIES = '__total__';

const LABEL_MIN_FRACTION = 0.05;

const ZOOM_THROTTLE = 60;

/* Mirrors the SI formatting src/command/report.rs applies to the tables. */
function si(value) {
  const steps = [[1e12, 'T'], [1e9, 'G'], [1e6, 'M'], [1e3, 'k']];
  for (const step of steps) {
    if (Math.abs(value) >= step[0]) return (value / step[0]).toFixed(2) + step[1];
  }
  return value.toFixed(0);
}

function contrastText(hex) {
  const channel = (offset) => {
    const value = parseInt(hex.slice(offset, offset + 2), 16) / 255;
    return value <= 0.03928 ? value / 12.92 : Math.pow((value + 0.055) / 1.055, 2.4);
  };
  const luminance = 0.2126 * channel(1) + 0.7152 * channel(3) + 0.0722 * channel(5);
  return luminance > 0.45 ? '#1a1a1a' : '#f5f5f5';
}

const hexAlpha = (hex, alpha) =>
  hex + Math.round(alpha * 255).toString(16).padStart(2, '0');

function niceMax(value) {
  if (!(value > 0)) return 1;
  const step = Math.pow(10, Math.floor(Math.log10(value))) / 2;
  return Math.ceil(value / step) * step;
}

/* Cells carry their own padding because the page stylesheet does not reach into a tooltip. */
function tooltipTable(head, rows) {
  const cell = (value) =>
    '<td style="text-align:right;padding:1px 0 1px 14px;white-space:nowrap">' + value + '</td>';
  const cells = rows
    .map(
      (row) =>
        '<tr><td style="padding:1px 0;white-space:nowrap">' +
        row[0] +
        '</td>' +
        row.slice(1).map(cell).join('') +
        '</tr>'
    )
    .join('');
  return (
    head +
    '<table style="border-collapse:collapse;margin-top:2px;line-height:1.35">' +
    cells +
    '</table>'
  );
}

const AXIS = {
  axisLine: { lineStyle: { color: THEME.border } },
  axisTick: { show: false },
  axisLabel: { color: THEME.muted, fontSize: 11 },
  nameTextStyle: { color: THEME.faint, fontSize: 11 },
  splitLine: { show: false },
};

const BASE = {
  animationDuration: 300,
  animationEasing: 'cubicOut',
  textStyle: { color: THEME.foreground, fontFamily: FONT },
  legend: {
    top: 0,
    icon: 'circle',
    itemWidth: 10,
    itemHeight: 10,
    textStyle: { color: THEME.muted },
    // Both states drop the border the series carries, which would otherwise draw the enabled swatch
    // one surface-coloured ring smaller than the disabled one.
    itemStyle: { borderWidth: 0 },
    inactiveColor: THEME.faint,
    inactiveBorderColor: 'transparent',
    inactiveBorderWidth: 0,
  },
  tooltip: {
    confine: true,
    backgroundColor: THEME.elevated,
    borderColor: THEME.border,
    borderWidth: 1,
    borderRadius: 8,
    padding: [8, 12],
    textStyle: { color: THEME.foreground, fontSize: 12 },
    axisPointer: { lineStyle: { color: THEME.faint, type: 'dashed' } },
  },
};

const LABELS = DATA.guests.map((guest) => guest.label);

/* Narrowed by the composition legend, so every figure below is over the kinds still stacked. */
let enabledKinds = new Set(DATA.kinds);

let showCost = false;

const visibleTotal = (guest) =>
  DATA.kinds.reduce(
    (sum, kind, index) => (enabledKinds.has(kind) ? sum + guest.components[index] : sum),
    0
  );

const visibleTotals = () => DATA.guests.map((guest) => visibleTotal(guest));

const visiblePeak = () => Math.max.apply(null, visibleTotals()) || 1;

function kindLabel(point) {
  if (point.value / visiblePeak() < LABEL_MIN_FRACTION) return '';
  if (showCost) return si(point.value);
  return Math.round((point.value / visibleTotal(DATA.guests[point.dataIndex])) * 100) + '%';
}

/* Cost beside the ratio to the cheapest guest, so dropping a kind shows the comparison without it. */
function totalLabel(point) {
  const total = visibleTotal(DATA.guests[point.dataIndex]);
  const best = Math.min.apply(null, visibleTotals());
  return si(total) + ' / ' + (best > 0 ? (total / best).toFixed(2) + 'x' : '-');
}

const kindSeries = DATA.kinds.map((kind, index) => ({
  name: kind,
  type: 'bar',
  stack: 'cost',
  barWidth: 24,
  // The surface-coloured border is the gap that keeps neighbours from reading as one fill.
  itemStyle: { color: at(KIND_COLORS, index), borderColor: THEME.surface, borderWidth: 1 },
  label: {
    show: true,
    fontSize: 11,
    color: contrastText(at(KIND_COLORS, index)),
    formatter: kindLabel,
  },
  data: DATA.guests.map((guest) => guest.components[index]),
}));

/* Zero-width spacer closing each stack, so the row total prints past the end of the bar. */
const totalSeries = {
  name: TOTAL_SERIES,
  type: 'bar',
  stack: 'cost',
  silent: true,
  itemStyle: { color: 'transparent' },
  label: {
    show: true,
    position: 'right',
    fontSize: 11,
    color: THEME.muted,
    formatter: totalLabel,
  },
  data: DATA.guests.map(() => 0),
};

const composition = {
  ...BASE,
  animation: false,
  legend: { ...BASE.legend, data: DATA.kinds },
  grid: { left: 8, right: 96, top: 34, bottom: 8, containLabel: true },
  xAxis: {
    ...AXIS,
    type: 'value',
    max: visiblePeak(),
    axisLabel: { ...AXIS.axisLabel, formatter: si },
  },
  yAxis: {
    ...AXIS,
    type: 'category',
    inverse: true,
    data: LABELS,
    axisLabel: { ...AXIS.axisLabel, fontSize: 12 },
  },
  tooltip: {
    ...BASE.tooltip,
    trigger: 'axis',
    axisPointer: { type: 'shadow' },
    formatter: (params) => {
      const guest = DATA.guests[params[0].dataIndex];
      const total = visibleTotal(guest);
      const rows = params
        .filter((point) => point.seriesName !== TOTAL_SERIES)
        .map((point) => [
          point.marker + point.seriesName,
          si(point.value),
          ((point.value / total) * 100).toFixed(1) + '%',
        ]);
      return tooltipTable(guest.label, rows);
    },
  },
  series: kindSeries.concat([totalSeries]),
};

const GAS_POINTS = DATA.lines.reduce((all, line) => all.concat(line.points), []);
const COST_CEILING = niceMax(Math.max.apply(null, GAS_POINTS.map((point) => point[1])));

/*
 * The cost axis is pinned to the whole corpus and animation is off, so hiding a guest or zooming
 * never rescales or slides what is left. The gas axis stays unpinned because dataZoom drives it.
 */
const gas = {
  ...BASE,
  animation: false,
  grid: { left: 24, right: 24, top: 32, bottom: 76, containLabel: true },
  xAxis: {
    ...AXIS,
    type: 'value',
    min: 'dataMin',
    max: 'dataMax',
    axisLabel: { ...AXIS.axisLabel, formatter: si },
  },
  yAxis: {
    ...AXIS,
    type: 'value',
    min: 0,
    max: COST_CEILING,
    axisLabel: { ...AXIS.axisLabel, formatter: si },
  },
  // filterMode none keeps the points outside the window, so a line still draws across both edges.
  dataZoom: [
    {
      type: 'inside',
      filterMode: 'none',
      minSpan: 1,
      zoomOnMouseWheel: true,
      moveOnMouseMove: false,
      throttle: ZOOM_THROTTLE,
    },
    {
      type: 'slider',
      filterMode: 'none',
      minSpan: 1,
      throttle: ZOOM_THROTTLE,
      height: 30,
      bottom: 24,
      borderColor: THEME.border,
      backgroundColor: 'transparent',
      fillerColor: hexAlpha(THEME.primary, 0.18),
      handleStyle: { color: THEME.primary, borderColor: THEME.primary },
      moveHandleStyle: { color: THEME.primary },
      textStyle: { color: THEME.muted, fontSize: 10 },
      dataBackground: {
        lineStyle: { color: THEME.border, opacity: 0.5 },
        areaStyle: { color: THEME.border, opacity: 0.08 },
      },
      selectedDataBackground: {
        lineStyle: { color: THEME.primary, opacity: 0.6 },
        areaStyle: { color: THEME.primary, opacity: 0.08 },
      },
    },
  ],
  tooltip: {
    ...BASE.tooltip,
    trigger: 'axis',
    formatter: (params) => {
      const head = 'Block #' + params[0].value[2] + ' - ' + si(params[0].value[0]) + ' gas';
      const rows = params.map((point) => [point.marker + point.seriesName, si(point.value[1])]);
      return tooltipTable(head, rows);
    },
  },
  // A point per block, so markers stay hidden until the axis pointer picks one out.
  series: DATA.lines.map((line, index) => ({
    name: line.label,
    type: 'line',
    symbol: 'circle',
    symbolSize: 8,
    showSymbol: false,
    lineStyle: { width: 2, color: at(LINE_COLORS, index) },
    itemStyle: { color: at(LINE_COLORS, index), borderColor: THEME.surface, borderWidth: 2 },
    data: line.points,
  })),
};

function mount(id, option, height) {
  const element = document.getElementById(id);
  element.style.height = height + 'px';
  const chart = echarts.init(element);
  chart.setOption(option);
  return chart;
}

/* The legend is canvas-drawn and its select event carries no modifier, so the click is read on its
   way down instead. */
let multiSelect = false;
document.addEventListener(
  'click',
  (event) => {
    multiSelect = event.metaKey || event.ctrlKey;
  },
  true
);

/*
 * Grafana-style legend. A plain click isolates one series, a plain click on the lone remaining
 * series resets to all, and a Cmd or Ctrl click keeps the default per-series toggle. The rule reads
 * the selection the click started from, so it holds however the current state was reached.
 */
function isolatingLegend(chart, names, onChange) {
  chart.on('legendselectchanged', (params) => {
    if (multiSelect) {
      onChange(params.selected);
      return;
    }
    const before = names.filter((name) =>
      name === params.name ? !params.selected[name] : params.selected[name]
    );
    const reset = before.length === 1 && before[0] === params.name;
    const selected = {};
    names.forEach((name) => {
      selected[name] = reset || name === params.name;
    });
    chart.setOption({ legend: { selected } });
    onChange(selected);
  });
}

const compositionChart = mount(
  'composition',
  composition,
  Math.max(180, DATA.guests.length * 46 + 72)
);
const gasChart = mount('gas', gas, 420);

/* Merges into the existing series, so the bars are not rebuilt on every toggle. */
function refreshComposition() {
  compositionChart.setOption({
    xAxis: { max: visiblePeak() },
    series: DATA.kinds
      .map(() => ({ label: { formatter: kindLabel } }))
      .concat([{ label: { formatter: totalLabel } }]),
  });
}

isolatingLegend(compositionChart, DATA.kinds, (selected) => {
  enabledKinds = new Set(DATA.kinds.filter((kind) => selected[kind]));
  refreshComposition();
});

isolatingLegend(
  gasChart,
  DATA.lines.map((line) => line.label),
  () => {}
);

/* Only fires on a bar, never on empty grid space. */
compositionChart.on('click', () => {
  showCost = !showCost;
  refreshComposition();
});

function sortable(table) {
  const headers = Array.prototype.slice.call(table.querySelectorAll('th[data-sort]'));
  const body = table.tBodies[0];
  const rows = Array.prototype.slice.call(body.rows);
  const apply = (header, direction) => {
    const column = header.cellIndex;
    const numeric = header.dataset.sort === 'number';
    const key = (row) => row.cells[column].textContent.trim();
    const ordered = rows.slice().sort((left, right) => {
      const order = numeric
        ? parseFloat(key(left)) - parseFloat(key(right))
        : key(left).localeCompare(key(right));
      return direction === 'ascending' ? order : -order;
    });
    headers.forEach((other) => other.removeAttribute('aria-sort'));
    header.setAttribute('aria-sort', direction);
    ordered.forEach((row) => body.appendChild(row));
  };
  headers.forEach((header) => {
    header.addEventListener('click', () => {
      apply(header, header.getAttribute('aria-sort') === 'ascending' ? 'descending' : 'ascending');
    });
  });
  apply(headers[0], 'ascending');
}

sortable(document.getElementById('cost'));

const charts = [compositionChart, gasChart];
window.addEventListener('resize', () => charts.forEach((chart) => chart.resize()));
