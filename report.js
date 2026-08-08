/* Published cost report. One JSON per zkVM is written by `zkevm-prof report` and published beside
   this file, so a tab is one workflow run's batch and never mixes results from two of them. */

const ZKVMS = ['openvm', 'sp1', 'zisk'];

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

/* Mirrors the SI formatting src/command/report.rs applies to the markdown tables. */
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

function mount(id, option, height) {
  const element = document.getElementById(id);
  element.style.height = height + 'px';
  const chart = echarts.init(element);
  chart.setOption(option);
  return chart;
}

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
    header.onclick = () => {
      apply(header, header.getAttribute('aria-sort') === 'ascending' ? 'descending' : 'ascending');
    };
  });
  apply(headers[0], 'ascending');
}

/* Charts of the tab on screen, disposed before the next tab builds its own. */
let charts = [];

function clearCharts() {
  charts.forEach((chart) => chart.dispose());
  charts = [];
}

function renderRun(data) {
  const when = new Date(data.generated_at * 1000);
  const stamp = when.toISOString().replace('T', ' ').slice(0, 16) + ' UTC';
  const profiled = data.run_url
    ? '<a href="' + data.run_url + '">' + stamp + '</a>'
    : stamp;
  const fields = [
    ['Profiled', profiled],
    ['zkVM', data.zkvm],
    ['zkVM version', data.zkvm_version],
    ['Guests', String(data.guests.length)],
    ['Blocks', String(data.blocks)],
  ];
  document.getElementById('run').innerHTML = fields
    .map((field) => '<div><dt>' + field[0] + '</dt><dd>' + field[1] + '</dd></div>')
    .join('');
}

/* Width a short commit hash and a semver tag both come to, so the two identifiers a guest is
   normally pinned by survive whole and only a descriptive name is cut. */
const VERSION_LIMIT = 7;

/* A cut version keeps the whole string as the cell's label, which names it for a reader on assistive
   technology and is the text hovering grows it back to, so the column never widens to hold it. */
function versionCell(version) {
  if (version.length <= VERSION_LIMIT) return '<td>' + version + '</td>';
  return '<td aria-label="' + version + '">' + version.slice(0, VERSION_LIMIT) + '\u2026</td>';
}

function renderTable(data) {
  const body = document.getElementById('cost').tBodies[0];
  body.innerHTML = data.guests
    .map((guest) =>
      '<tr><td>' +
      guest.label +
      '</td>' +
      versionCell(guest.guest_version) +
      '<td>' +
      si(guest.total) +
      '</td><td>' +
      guest.relative.toFixed(2) +
      'x</td></tr>'
    )
    .join('');
  sortable(document.getElementById('cost'));
}

function renderComposition(data) {
  const card = document.getElementById('composition-card');
  /* A zkVM that prices an execution as one number leaves the total nothing to be split into, and
     the page drops the composition section along with it. */
  if (!data.kinds.length) {
    card.classList.add('hidden');
    return null;
  }
  card.classList.remove('hidden');

  /* Each tab is published by its own run, so a report written before the notes existed loads
     alongside one that carries them and simply goes without a legend. */
  const notes = data.notes ?? [];
  document.getElementById('composition-note').innerHTML = data.kinds
    .map((kind, index) => (notes[index] ? `<li><code>${kind}</code>: ${notes[index]}</li>` : ''))
    .join('');

  let enabledKinds = new Set(data.kinds);
  let showCost = false;

  const visibleTotal = (guest) =>
    data.kinds.reduce(
      (sum, kind, index) => (enabledKinds.has(kind) ? sum + guest.components[index] : sum),
      0
    );
  const visibleTotals = () => data.guests.map((guest) => visibleTotal(guest));
  const visiblePeak = () => Math.max.apply(null, visibleTotals()) || 1;

  const kindLabel = (point) => {
    if (point.value / visiblePeak() < LABEL_MIN_FRACTION) return '';
    if (showCost) return si(point.value);
    return Math.round((point.value / visibleTotal(data.guests[point.dataIndex])) * 100) + '%';
  };

  /* Cost beside the ratio to the cheapest guest, so dropping a kind shows the comparison without
     it. */
  const totalLabel = (point) => {
    const total = visibleTotal(data.guests[point.dataIndex]);
    const best = Math.min.apply(null, visibleTotals());
    return si(total) + ' / ' + (best > 0 ? (total / best).toFixed(2) + 'x' : '-');
  };

  const kindSeries = data.kinds.map((kind, index) => ({
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
    data: data.guests.map((guest) => guest.components[index]),
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
    data: data.guests.map(() => 0),
  };

  const option = {
    ...BASE,
    animation: false,
    legend: { ...BASE.legend, data: data.kinds },
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
      data: data.guests.map((guest) => guest.label),
      axisLabel: { ...AXIS.axisLabel, fontSize: 12 },
    },
    tooltip: {
      ...BASE.tooltip,
      trigger: 'axis',
      axisPointer: { type: 'shadow' },
      formatter: (params) => {
        const guest = data.guests[params[0].dataIndex];
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

  const chart = mount('composition', option, Math.max(180, data.guests.length * 46 + 72));

  /* Merges into the existing series, so the bars are not rebuilt on every toggle. */
  const refresh = () => {
    chart.setOption({
      xAxis: { max: visiblePeak() },
      series: data.kinds
        .map(() => ({ label: { formatter: kindLabel } }))
        .concat([{ label: { formatter: totalLabel } }]),
    });
  };

  isolatingLegend(chart, data.kinds, (selected) => {
    enabledKinds = new Set(data.kinds.filter((kind) => selected[kind]));
    refresh();
  });

  /* Only fires on a bar, never on empty grid space. */
  chart.on('click', () => {
    showCost = !showCost;
    refresh();
  });

  return chart;
}

function renderGas(data) {
  const points = data.lines.reduce((all, line) => all.concat(line.points), []);
  const ceiling = niceMax(Math.max.apply(null, points.map((point) => point[1])));

  /*
   * The cost axis is pinned to the whole corpus and animation is off, so hiding a guest or zooming
   * never rescales or slides what is left. The gas axis stays unpinned because dataZoom drives it.
   */
  const option = {
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
      max: ceiling,
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
    series: data.lines.map((line, index) => ({
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

  const chart = mount('gas', option, 420);
  isolatingLegend(chart, data.lines.map((line) => line.label), () => {});
  return chart;
}

function render(data) {
  clearCharts();
  renderRun(data);
  renderTable(data);
  charts = [renderComposition(data), renderGas(data)].filter(Boolean);
}

function showError(message) {
  clearCharts();
  ['run-card', 'cost-card', 'composition-card', 'gas-card'].forEach((id) => {
    document.getElementById(id).classList.add('hidden');
  });
  const error = document.getElementById('error');
  error.textContent = message;
  error.hidden = false;
}

function showCards() {
  ['run-card', 'cost-card', 'gas-card'].forEach((id) => {
    document.getElementById(id).classList.remove('hidden');
  });
  document.getElementById('error').hidden = true;
}

/* Reports already loaded, so switching back to a tab redraws without fetching again. */
const loaded = new Map();

/* Tab on screen, which the hash listener reads to tell a real navigation from its own write. */
let current = null;

async function select(zkvm) {
  current = zkvm;
  Array.prototype.slice.call(document.querySelectorAll('#tabs button')).forEach((button) => {
    button.setAttribute('aria-selected', String(button.dataset.zkvm === zkvm));
  });
  if (location.hash.slice(1) !== zkvm) location.hash = zkvm;

  if (!loaded.has(zkvm)) {
    try {
      const response = await fetch(zkvm + '.json', { cache: 'no-store' });
      if (!response.ok) throw new Error(response.status + ' ' + response.statusText);
      loaded.set(zkvm, await response.json());
    } catch (cause) {
      showError('No report published for ' + zkvm + ' yet (' + cause.message + ').');
      return;
    }
  }
  showCards();
  // A page cached against a different version of this script would otherwise fail partway through
  // drawing and leave the reader a blank screen with no way to tell why.
  try {
    render(loaded.get(zkvm));
  } catch (cause) {
    showError('Could not draw the ' + zkvm + ' report (' + cause.message + '). Reload to retry.');
  }
}

function buildTabs() {
  document.getElementById('tabs').innerHTML = ZKVMS.map(
    (zkvm) =>
      '<button type="button" role="tab" data-zkvm="' +
      zkvm +
      '" aria-selected="false">' +
      zkvm +
      '</button>'
  ).join('');
  Array.prototype.slice.call(document.querySelectorAll('#tabs button')).forEach((button) => {
    button.addEventListener('click', () => select(button.dataset.zkvm));
  });
}

/* Back and forward move between tabs, and a link opened in an already loaded page selects the tab
   it names. */
window.addEventListener('hashchange', () => {
  const zkvm = location.hash.slice(1);
  if (ZKVMS.includes(zkvm) && zkvm !== current) select(zkvm);
});

/* Left and right step through the tabs. The step is read off the array, so either end simply has no
   neighbour to move to and the selection stops there rather than wrapping. */
window.addEventListener('keydown', (event) => {
  const step = { ArrowLeft: -1, ArrowRight: 1 }[event.key];
  if (!step || event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
  const next = ZKVMS[ZKVMS.indexOf(current) + step];
  if (!next) return;
  event.preventDefault();
  select(next);
  // Focus follows only when it was already on a tab, so a reader partway down the page keeps theirs.
  const tabs = document.getElementById('tabs');
  if (document.activeElement && document.activeElement.parentElement === tabs) {
    tabs.querySelector('button[data-zkvm="' + next + '"]').focus();
  }
});

buildTabs();
select(ZKVMS.includes(location.hash.slice(1)) ? location.hash.slice(1) : ZKVMS[0]);

window.addEventListener('resize', () => charts.forEach((chart) => chart.resize()));
