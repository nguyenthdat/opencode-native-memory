export interface RelevanceJudgment {
  fixture_id: string;
  grade: number;
}

export interface QueryRanking {
  answerable: boolean;
  returned: string[];
  relevance: RelevanceJudgment[];
}

export interface RetrievalMetrics {
  query_count: number;
  precision_at: Record<string, number>;
  recall_at: Record<string, number>;
  hit_at: Record<string, number>;
  mrr_at_10: number;
  ndcg_at_10: number;
  answerable_coverage: number;
  false_abstention_rate: number;
  no_answer_abstention_specificity: number;
}

const CUTOFFS = [1, 3, 5, 10] as const;

export function computeRetrievalMetrics(rankings: readonly QueryRanking[]): RetrievalMetrics {
  const answerable = rankings.filter((ranking) => ranking.answerable);
  const precisionAt: Record<string, number> = {};
  const recallAt: Record<string, number> = {};
  const hitAt: Record<string, number> = {};
  for (const cutoff of CUTOFFS) {
    precisionAt[String(cutoff)] = mean(answerable.map((ranking) => precisionAtK(ranking, cutoff)));
    recallAt[String(cutoff)] = mean(answerable.map((ranking) => recallAtK(ranking, cutoff)));
    hitAt[String(cutoff)] = mean(answerable.map((ranking) => hitAtK(ranking, cutoff)));
  }
  const noAnswer = rankings.filter((ranking) => !ranking.answerable);
  const covered = answerable.filter((ranking) => ranking.returned.length > 0).length;
  const correctlyAbstained = noAnswer.filter((ranking) => ranking.returned.length === 0).length;
  return {
    query_count: rankings.length,
    precision_at: precisionAt,
    recall_at: recallAt,
    hit_at: hitAt,
    mrr_at_10: mean(answerable.map((ranking) => reciprocalRank(ranking, 10))),
    ndcg_at_10: mean(answerable.map((ranking) => ndcg(ranking, 10))),
    answerable_coverage: ratio(covered, answerable.length),
    false_abstention_rate: ratio(answerable.length - covered, answerable.length),
    no_answer_abstention_specificity: ratio(correctlyAbstained, noAnswer.length),
  };
}

function relevantIds(ranking: QueryRanking): Set<string> {
  return new Set(ranking.relevance.filter((item) => item.grade > 0).map((item) => item.fixture_id));
}

function precisionAtK(ranking: QueryRanking, cutoff: number): number {
  const relevant = relevantIds(ranking);
  const returned = ranking.returned.slice(0, cutoff);
  if (returned.length === 0) return ranking.answerable ? 0 : 1;
  return returned.filter((id) => relevant.has(id)).length / returned.length;
}

function recallAtK(ranking: QueryRanking, cutoff: number): number {
  const relevant = relevantIds(ranking);
  if (relevant.size === 0) return ranking.returned.length === 0 ? 1 : 0;
  const found = ranking.returned.slice(0, cutoff).filter((id) => relevant.has(id)).length;
  return found / relevant.size;
}

function hitAtK(ranking: QueryRanking, cutoff: number): number {
  const relevant = relevantIds(ranking);
  if (relevant.size === 0) return ranking.returned.length === 0 ? 1 : 0;
  return ranking.returned.slice(0, cutoff).some((id) => relevant.has(id)) ? 1 : 0;
}

function reciprocalRank(ranking: QueryRanking, cutoff: number): number {
  const relevant = relevantIds(ranking);
  const index = ranking.returned.slice(0, cutoff).findIndex((id) => relevant.has(id));
  if (index < 0) return relevant.size === 0 && ranking.returned.length === 0 ? 1 : 0;
  return 1 / (index + 1);
}

function ndcg(ranking: QueryRanking, cutoff: number): number {
  const grades = new Map(ranking.relevance.map((item) => [item.fixture_id, item.grade]));
  const actual = ranking.returned.slice(0, cutoff).map((id) => grades.get(id) ?? 0);
  const ideal = ranking.relevance
    .map((item) => item.grade)
    .sort((left, right) => right - left)
    .slice(0, cutoff);
  const idealScore = discountedGain(ideal);
  if (idealScore === 0) return ranking.returned.length === 0 ? 1 : 0;
  return discountedGain(actual) / idealScore;
}

function discountedGain(grades: readonly number[]): number {
  return grades.reduce((total, grade, index) => total + (2 ** grade - 1) / Math.log2(index + 2), 0);
}

function mean(values: readonly number[]): number {
  return ratio(
    values.reduce((total, value) => total + value, 0),
    values.length,
  );
}

function ratio(numerator: number, denominator: number): number {
  return denominator === 0 ? 0 : numerator / denominator;
}
