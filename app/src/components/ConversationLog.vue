<script setup lang="ts">
import { useGameStore, degreeLabel, statRollLine } from "../stores/game";
import type { StatRollView } from "../types/api";
import { t } from "../i18n";
import DiceReveal from "./DiceReveal.vue";
import DecisionPanel from "./DecisionPanel.vue";
import ContestPanel from "./ContestPanel.vue";

const game = useGameStore();

/** 加算式判定のダイス表記: 既定 1d20(5)、複数/乗数なら 3d6(合計11)×5 (2026-07-20)。 */
function diceLabel(c: { count: number; sides: number; roll: number; times: number }): string {
  if (c.count > 1 || c.times > 1) {
    const mult = c.times > 1 ? `×${c.times}` : "";
    return `${c.count}d${c.sides}(合計${c.roll})${mult}`;
  }
  return `1d${c.sides}(${c.roll})`;
}

/** 可変量ダイスの開帳ラベル (結果を含めない): 「player SAN 1d6」。 */
function statRollLabel(sr: StatRollView): string {
  const bonus = sr.bonus !== 0 ? (sr.bonus > 0 ? `+${sr.bonus}` : `${sr.bonus}`) : "";
  return `${sr.entity} ${sr.key} ${sr.count}d${sr.sides}${bonus}`;
}

/** 可変量ダイスの着地値 (出目の合計。amount は clamp 後の適用量なので出目とは別)。 */
function statRollFinal(sr: StatRollView): number {
  return sr.rolls.reduce((a, b) => a + b, 0);
}
</script>

<template>
  <!-- 本文フォントは container で inherit (空なら UI 既定のまま)。色+影は語り系要素にだけ当てる。 -->
  <!-- `selectable`: 語りは読んで写すためのテキストなので選択を許す (UI ラベルは main.css で
       一律 select-none)。 -->
  <div
    class="selectable flex-1 overflow-y-auto px-6 py-5 space-y-4"
    :style="{ fontFamily: game.messageFontFamily }"
  >
    <template v-for="(entry, i) in game.log" :key="i">
      <!-- 開幕描写 -->
      <p
        v-if="entry.kind === 'opening'"
        class="italic whitespace-pre-wrap leading-relaxed"
        :style="game.authoredStyle"
      >
        {{ entry.text }}
      </p>

      <!-- プレイヤーの行動 -->
      <div v-else-if="entry.kind === 'player'" class="flex justify-end">
        <div class="max-w-[80%] rounded-lg bg-ash/60 px-4 py-2 text-parchment/90">
          <span class="text-ember/70 text-xs mr-2">{{ t("log.you") }}</span>{{ entry.text }}
        </div>
      </div>

      <!-- GM の語り -->
      <p
        v-else-if="entry.kind === 'narration'"
        class="whitespace-pre-wrap leading-relaxed text-parchment"
        :style="game.narrationStyle"
      >
        {{ entry.text }}
      </p>

      <!-- 作者が YAML に書いた確定文 (結末文など)。GM の即興と色で分ける。 -->
      <p
        v-else-if="entry.kind === 'authored'"
        class="whitespace-pre-wrap leading-relaxed"
        :style="game.authoredStyle"
      >
        {{ entry.text }}
      </p>

      <!-- 反応ビート + 想起された伏線 (黒の透過背景 = 表示設定で濃さ調整、可読性の手当て) -->
      <div
        v-else-if="entry.kind === 'beat'"
        class="border-l-2 border-ember/60 pl-3 space-y-1 rounded-r py-1.5 pr-3"
        :style="game.beatBgStyle"
        :data-theme="game.beatBgOpacity > 0 ? 'dark' : null"
      >
        <p v-if="entry.narration.trim()" class="text-ember" :style="{ textShadow: game.narrationStyle.textShadow ?? '' }">✦ {{ entry.narration }}</p>
        <!-- 想起された伏線 (memoria) — 既定で畳む。次ターンに GM が語りへ織り込むので、生表示はメタ/ネタバレ気味 -->
        <template v-if="entry.recalled.length">
          <button
            type="button"
            class="inline-flex items-center leading-none text-glow/50 hover:text-glow/80 transition-colors text-sm"
            :title="entry.expanded ? t('log.recallHide') : t('log.recallShow')"
            :aria-expanded="entry.expanded ?? false"
            @click="entry.expanded = !entry.expanded"
          >
            ◈
          </button>
          <div v-if="entry.expanded" class="space-y-1 mt-1">
            <p
              v-for="(line, j) in entry.recalled"
              :key="j"
              class="text-glow/70 text-sm whitespace-pre-wrap pl-3 border-l border-ash"
            >
              {{ line }}
            </p>
          </div>
        </template>
      </div>

      <!-- ダイス (spec 18: 開帳済みの分だけ表示し、次の 1 個を伏せカードで出す) -->
      <div v-else-if="entry.kind === 'rolls'" class="space-y-0.5">
        <p v-for="(r, j) in entry.rolls.slice(0, entry.revealed)" :key="j" class="text-sm text-parchment/70">
          🎲 1d{{ r.sides }} = {{ r.result }} (DC {{ r.dc }}) →
          <span :class="r.success ? 'text-glow' : 'text-ember/60'">{{ r.success ? t("log.success") : t("log.fail") }}</span>
        </p>
        <DiceReveal
          v-if="entry.revealed < entry.rolls.length && i === game.revealTargetIndex"
          :label="`1d${entry.rolls[entry.revealed].sides} (DC ${entry.rolls[entry.revealed].dc})`"
          :final="entry.rolls[entry.revealed].result"
          :max="entry.rolls[entry.revealed].sides"
          @revealed="game.revealNext(i)"
        />
      </div>

      <!-- 技能判定 (加算式 = 出目+修正 vs DC / percentile = d100 ロールアンダー + 成功度, spec 16) -->
      <div v-else-if="entry.kind === 'checks'" class="space-y-1">
        <template v-for="(c, j) in entry.checks.slice(0, entry.revealed)" :key="j">
          <p v-if="c.degree" class="text-sm text-parchment/70">
            🎯 {{ t("log.checkLabel", { entity: c.entity, stat: c.stat }) }}: d100={{ c.roll }} {{ c.success ? "≤" : ">" }} {{ c.dc }} →
            <span :class="c.success ? 'text-glow' : 'text-ember/60'">{{ degreeLabel(c.degree) }}</span>
          </p>
          <p v-else class="text-sm text-parchment/70">
            🎯 {{ t("log.checkLabel", { entity: c.entity, stat: c.stat }) }}: {{ diceLabel(c) }}{{ c.modifier >= 0 ? "+" + c.modifier : c.modifier }} = {{ c.total }} (DC {{ c.dc }}) →
            <span :class="c.success ? 'text-glow' : 'text-ember/60'">{{ c.success ? t("log.success") : t("log.fail") }}</span>
          </p>
          <!-- authored 結末ナレーション (毎回・同ターン)。失敗を必ず描く。 -->
          <p v-if="c.narration" class="whitespace-pre-wrap" :style="game.authoredStyle">{{ c.narration }}</p>
        </template>
        <DiceReveal
          v-if="entry.revealed < entry.checks.length && i === game.revealTargetIndex"
          :label="t('log.checkLabel', { entity: entry.checks[entry.revealed].entity, stat: entry.checks[entry.revealed].stat })"
          :final="entry.checks[entry.revealed].roll"
          :max="entry.checks[entry.revealed].degree ? 100 : entry.checks[entry.revealed].sides * entry.checks[entry.revealed].count"
          @revealed="game.revealNext(i)"
        />
      </div>

      <!-- 可変量ダイス (roll_stat, spec 16): 「SAN -4 (1d6=4)」の監査行 -->
      <div v-else-if="entry.kind === 'statrolls'" class="space-y-0.5">
        <p v-for="(sr, j) in entry.stat_rolls.slice(0, entry.revealed)" :key="j" class="text-sm text-parchment/70">
          🎲 {{ statRollLine(sr) }}
        </p>
        <DiceReveal
          v-if="entry.revealed < entry.stat_rolls.length && i === game.revealTargetIndex"
          :label="statRollLabel(entry.stat_rolls[entry.revealed])"
          :final="statRollFinal(entry.stat_rolls[entry.revealed])"
          :max="entry.stat_rolls[entry.revealed].count * entry.stat_rolls[entry.revealed].sides"
          @revealed="game.revealNext(i)"
        />
      </div>

      <!-- 却下 (正本が嘘を弾いた) -->
      <div v-else-if="entry.kind === 'reject'" class="rounded-lg bg-ash/30 px-4 py-2 text-sm">
        <p class="text-ember/80">{{ t("log.rejectHeader", { attempts: entry.attempts }) }}</p>
        <ul class="list-disc list-inside text-parchment/60 mt-1">
          <li v-for="(reason, j) in entry.reasons" :key="j">{{ reason }}</li>
        </ul>
        <p class="text-parchment/40 mt-1">{{ t("log.rejectNote") }}</p>
      </div>

      <!-- 自己修復 (GM が筋を通すまでの試行) — 既定は ⚠ アイコンのみ。クリックで展開 (メタ情報の没入低下を避ける) -->
      <div v-else-if="entry.kind === 'selfrepair'" class="text-xs">
        <button
          type="button"
          class="inline-flex items-center leading-none text-warn/70 hover:text-warn transition-colors"
          :title="entry.expanded ? t('log.selfrepairHide') : t('log.selfrepairShow', { attempts: entry.attempts })"
          :aria-expanded="entry.expanded ?? false"
          @click="entry.expanded = !entry.expanded"
        >
          ⚠
        </button>
        <div v-if="entry.expanded" class="mt-1 rounded-lg bg-ash/20 px-4 py-2 text-parchment/55">
          <p class="text-parchment/45">{{ t("log.selfrepairBody", { attempts: entry.attempts }) }}</p>
          <ul class="list-disc list-inside mt-0.5">
            <li v-for="(reasons, j) in entry.reasons" :key="j">{{ t("log.selfrepairAttempt", { n: j + 1, reasons: reasons.join(" / ") }) }}</li>
          </ul>
        </div>
      </div>

      <!-- システム告知 -->
      <p v-else-if="entry.kind === 'system'" class="text-center text-glow/80 text-sm">
        {{ entry.text }}
      </p>
    </template>

    <!-- 決断パネル (spec 18 Phase B): 開帳がすべて済んだ失敗に「受け入れる/押す/買う」を出す -->
    <DecisionPanel />

    <!-- 対決パネル (spec 18 Phase C): 決着まで LLM を介さないラウンド制の交互振り -->
    <ContestPanel />

    <p v-if="game.loading" class="text-parchment/40 text-sm animate-pulse">
      {{
        game.writingEpilogue
          ? t("log.writingEpilogue")
          : game.compacting
            ? t("log.compacting")
            : t("log.thinking")
      }}
    </p>
  </div>
</template>
