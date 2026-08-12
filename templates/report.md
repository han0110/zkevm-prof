{#- The same figures as the page, as tables. Formatting comes from src/command/report.rs. -#}
# zkEVM guest cost profile

Profiles of {{ view.guests.len() }} guests over {{ view.blocks }} blocks, on {{ view.zkvm_version }}.

## Cost

| Guest | Version | Avg cost | Relative |
| --- | --- | ---: | ---: |
{% for guest in view.guests -%}
| {{ guest.label }} | {{ guest.version }} | {{ guest.total }} | {{ guest.relative }} |
{% endfor -%}
{% if !view.kinds.is_empty() %}
## Cost composition

| Guest |{% for kind in view.kinds %} {{ kind.name }} |{% endfor %}
| --- |{% for kind in view.kinds %} ---: |{% endfor %}
{% for guest in view.guests -%}
| {{ guest.label }} |{% for component in guest.components %} {{ component.value }} ({{ component.share }}%) |{% endfor %}
{% endfor %}
{% for kind in view.kinds -%}
- `{{ kind.name }}`: {{ kind.note }}
{% endfor -%}
{% endif -%}
{% if !view.heap.is_empty() %}
## Peak heap

Mean of the peaks a guest reached over the corpus.

| Guest | Version | Avg peak heap | Relative |
| --- | --- | ---: | ---: |
{% for guest in view.heap -%}
| {{ guest.label }} | {{ guest.version }} | {{ guest.peak }} | {{ guest.relative }} |
{% endfor -%}
{% endif -%}
