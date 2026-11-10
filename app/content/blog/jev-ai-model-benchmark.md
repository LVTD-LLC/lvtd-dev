+++
title = "Jev benchmark: the best AI models by task and cost"
slug = "jev-ai-model-benchmark"
description = "MiMo Flash leads our six-task benchmark on both Elo and value. See which models win for writing, coding, math, advice, analysis, and synthesis."
author = "Rasul Kireev"
date = "2026-09-28"
draft = false
+++

**Start with MiMo-V2.6-Flash.** It tops our 29-model benchmark on both overall Elo and the score that balances Elo with cost. But if you mainly need code, mathematical reasoning, or incident analysis, a task specialist deserves a look.

We compared 174 answers across six tasks, using TypeSafe's Jev as the judge. The results below are a shortlist for your own testing, not a claim that any model is universally best.

Best overall in this run**MiMo Flash**1,699.3 Elo

Best Elo/cost balance**MiMo Flash**76.5 / 100 Value v1

Mean saved-answer cost**$0.00286**For MiMo Flash across six tasks

[Overall](#results) · [By task](#tasks) · [Best value](#value) · [Your shortlist](#choose) · [Methodology](#setup)

## Best overall: MiMo Flash

MiMo Flash wins the writing task and places second in coding, mathematics, data analysis, and evidence synthesis. Its weak spot is personal advice: thirteenth. That makes it a strong starting point for a mixed workload, but not the automatic choice for every job.

Overall Elo: the leading six models

1. **01** MiMo-V2.6-Flash**1,699.3**
2. **02** Claude Fable 5.1**1,682.8**
3. **03** MiMo V2.6 Pro**1,681.2**
4. **04** Claude Opus 5.5**1,663.9**
5. **05** GLM 5.3 Prime**1,647.8**
6. **06** GLM 5.3 Flash**1,608.0**

Higher is better. Bar scale: 0-1,800 Elo. The overall rating averages six equally weighted tasks; Elo is not an accuracy percentage.

Claude Fable 5.1 and MiMo V2.6 Pro come next, separated by only 1.6 Elo. We would test both rather than treat that tiny gap as a meaningful difference. Fable's saved answers also cost much more, which changes the shortlist once price enters the picture.

## Best by task: match the model to the work

Each category below contains **one prompt**. These are the winning answers in this experiment, not broad verdicts on writing, coding, or reasoning ability.

Six tasks. Five different winners.

* Writing**MiMo-V2.6-Flash**1,780.2 Elo[A modular-monolith recommendation →](/jev-benchmark/writing-the-case-for-a-modular-monolith)MiMo Flash: #1 of 29
* Coding**MiMo V2.6 Pro**1,721.7 Elo[Offline dynamic connectivity →](/jev-benchmark/coding-offline-dynamic-connectivity)MiMo Flash: #2 of 29
* Mathematics**GLM 5.3 Prime**1,782.9 Elo[A probability derivation →](/jev-benchmark/math-stopping-time-and-variance)MiMo Flash: #2 of 29
* Personal advice**GLM 5.3 Prime**1,724.4 Elo[A friendship under strain →](/jev-benchmark/personal-friendship-compassion-and-boundaries)MiMo Flash: #13 of 29
* Data analysis**Grok 4.7**1,756.5 Elo[A support-team rollout decision →](/jev-benchmark/support-rollout-analysis-v1)MiMo Flash: #2 of 29
* Evidence synthesis**Claude Opus 5.5**1,757.3 Elo[A source-cited incident brief →](/jev-benchmark/incident-evidence-synthesis-v1)MiMo Flash: #2 of 29

**For coding, start by comparing MiMo Pro with Flash.** For math or personal advice, inspect GLM Prime. For the support-rollout analysis, look at Grok 4.7; for turning conflicting incident notes into a brief, look at Claude Opus 5.5.

The advice result is effectively a photo finish: GLM Prime, Claude Fable, and Muse Spark are all within half an Elo point. Read their answers before picking one. And run the code or check the math yourself: a judge's preference is not proof of correctness.

## Best combined Elo and cost: MiMo Flash

Our **Value v1** score combines 70% Elo-derived quality with 30% affordability. MiMo Flash leads at **76.5**, followed by Muse Spark 1.3 Contributor at **70.5**. Muse is worth a look when price matters: its mean saved-answer cost is about $0.00116, although its overall Elo is lower.

Value v1: the leading six models

Elo contribution (up to 70) Affordability (up to 30)

1. MiMo-V2.6-Flash**76.5**

   1,699.3 Elo · $0.00286 / answer
2. Muse Spark 1.3 Contributor**70.5**

   1,587.6 Elo · $0.00116 / answer
3. Space Bunny Alpha**70.0**

   1,549.6 Elo · $0.00000 / answer
4. Nemotron 3 Ultra (free)**66.4**

   1,513.8 Elo · $0.00000 / answer
5. GLM 5.3 Flash**66.4**

   1,608.0 Elo · $0.00439 / answer
6. MiMo V2.6 Pro**66.4**

   1,681.2 Elo · $0.01055 / answer

Bar scale: 0-100. Costs are mean USD per saved answer. Nemotron, GLM Flash, and MiMo Pro all round to 66.4; their order uses unrounded scores, not a demonstrated meaningful difference.

Space Bunny Alpha and Nemotron benefit from reported $0 answers. That is a price observed in this run, not a promise of permanent free access. The value ranking can change when prices, tasks, or your priorities change.

### What does the price gap look like?

Mean cost per saved answer: three overall contenders

1. MiMo-V2.6-Flash**$0.00286**
2. MiMo V2.6 Pro**$0.01055**
3. Claude Fable 5.1**$0.22629**

Linear scale: $0-$0.22629 per answer. Fable's mean is about 79 times Flash's for these responses. Costs exclude failed attempts, retries, and judging charges.

That does not make Flash "79 times better." It means Flash is a sensible first model to test before paying more. A pricier model earns its place when it produces better results on *your* tasks.

## Build your shortlist

1. **Start with Flash for mixed work.** It combines the highest overall preference rating with a low observed cost.
2. **Add your task winner.** Compare its answer with Flash's, using a few real examples from your own workload.
3. **Set your quality floor, then compare cost.** If neither answer meets your requirements, a better value score will not fix that.

[Compare the models and read their answers →](/jev-benchmark)

## How we measured it

OpenRouter generated one saved answer per model per task. Jev (`jev-1.13.0`) then chose between every pair, using each prompt's rubric and without supplied model labels. That gives 406 pairs per task and **2,436 comparisons** overall. No model had tools or web access.

We replay the decisions in a stable order, starting each model at 1,500 Elo per question with K = 32. Overall Elo is the weighted mean of task ratings; all six tasks here carry equal weight. Costs use the same weighting, not the sum of all six answers.

The Value v1 formula, and how to change the balance

We put Elo and cost on comparable scales before combining them:

* `Q = 1 / (1 + 10^((1500 - Elo) / 400))`
* `A = 1 / (1 + cost / 0.01)`
* `Value v1 = 100 × (0.7 × Q + 0.3 × A)`

Cost is mean USD per saved answer. A model at 1,500 Elo and $0.01 scores 50. Only complete results with known costs receive a score; unknown cost is not free.

Prefer another tradeoff? Download the data and use `100 × (w × Q + (1 - w) × A)`, with w between 0 and 1. At w = 0.9, quality gets 90% and affordability 10%. This is your own calculation; the explorer's Value v1 stays fixed at 70/30.

### What this benchmark cannot tell you

Six prompts and one judge cannot establish general model ability. The 2,436 decisions reuse the same 174 answers: they are not independent examples. We did not repeat generations, swap every pair's presentation order, validate every answer, or calculate uncertainty intervals. Elo and Value v1 are neither accuracy nor return on investment.

Generation budgets also differ. The analysis and synthesis prompts request low reasoning, while the other four use provider defaults. Mistral maps low reasoning to its supported minimal mode; truncation retries can increase output limits. Read the saved settings before treating these as equal-compute comparisons.

**Data:** completed six-task snapshot, September 28, 2026 at 18:59 UTC; rechecked September 29. The [public JSON](/jev-benchmark/data.json) and [full methodology](/jev-benchmark/methodology.md) include prompts, answers, judgments, costs, and replay rules. The live explorer may move beyond this snapshot.

Snapshot fingerprint

`a96fcfca7f7ff09731476ca0b07e0601de2a66d269541765c2bfaa50f2808c6e`