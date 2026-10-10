#!/usr/bin/env python3
"""Checks that `sniff-rs graph` finds similar documents in languages written without spaces.

    python3 tools/check_cjk.py [--bin target/release/sniff-rs] [--baseline OTHER_BIN] [--docs 10]

For Chinese, Japanese, Korean and Thai, four topics (health, money, school,
travel) each get `--docs` short documents of random topic words joined the
way the language is written, with function words between them and no
spaces. Related documents share words, so each document should be linked
by `similar_to` to documents of its own topic and to none of another. The
checker prints, per language, how many documents have a same-topic link
(recall) and how many links cross topics (errors). With `--baseline` it
prints the same for another binary, to show the difference.
"""
import argparse
import json
import random
import subprocess
import tempfile
from pathlib import Path

TOPICS = {
    "zh": {
        "health": "医院 医生 患者 治疗 药物 手术 护士 诊断 症状 疫苗 门诊 康复".split(),
        "money": "银行 贷款 利率 存款 投资 股票 基金 保险 汇率 债券 理财 账户".split(),
        "school": "学校 老师 学生 课程 考试 教材 大学 毕业 作业 教室 学期 成绩".split(),
        "travel": "汽车 地铁 公路 火车 机场 航班 驾驶 高速 公交 车站 旅客 路线".split(),
    },
    "ja": {
        "health": "病院 医者 患者 治療 薬 手術 看護師 診断 症状 ワクチン 外来 回復".split(),
        "money": "銀行 融資 金利 預金 投資 株式 保険 為替 債券 ファンド 口座 資産".split(),
        "school": "学校 先生 学生 授業 試験 教科書 大学 卒業 宿題 教室 学期 成績".split(),
        "travel": "自動車 地下鉄 道路 電車 空港 航空便 運転 高速道路 バス 駅 旅行者 経路".split(),
    },
    "ko": {
        "health": "병원 의사 환자 치료 약물 수술 간호사 진단 증상 백신 외래 회복".split(),
        "money": "은행 대출 금리 예금 투자 주식 보험 환율 채권 펀드 계좌 자산".split(),
        "school": "학교 선생님 학생 수업 시험 교과서 대학 졸업 숙제 교실 학기 성적".split(),
        "travel": "자동차 지하철 도로 기차 공항 항공편 운전 고속도로 버스 정류장 여행객 경로".split(),
    },
    "th": {
        "health": "โรงพยาบาล แพทย์ ผู้ป่วย การรักษา ยา ผ่าตัด พยาบาล วินิจฉัย อาการ วัคซีน".split(),
        "money": "ธนาคาร สินเชื่อ ดอกเบี้ย เงินฝาก การลงทุน หุ้น ประกัน อัตราแลกเปลี่ยน พันธบัตร บัญชี".split(),
        "school": "โรงเรียน ครู นักเรียน หลักสูตร การสอบ ตำราเรียน มหาวิทยาลัย การบ้าน ห้องเรียน เกรด".split(),
        "travel": "รถยนต์ รถไฟฟ้า ถนน รถไฟ สนามบิน เที่ยวบิน ขับรถ ทางด่วน รถเมล์ สถานี".split(),
    },
}
FUNCTION = {
    "zh": ["的", "是", "在", "和", "了", "也", "有", "我们"],
    "ja": ["の", "は", "を", "に", "が", "と", "です", "ます"],
    "ko": ["은", "는", "이", "가", "을", "를", "에서", "입니다"],
    "th": ["และ", "ของ", "ใน", "ที่", "เป็น", "ได้", "มี", "การ"],
}
STOP = {"zh": "。", "ja": "。", "ko": ". ", "th": " "}


def doc(rng, lang, words):
    out = []
    for i in range(rng.randint(30, 50)):
        out.append(rng.choice(words))
        r = rng.random()
        if r < 0.5:
            out.append(rng.choice(FUNCTION[lang]))
        if i % 9 == 8:
            out.append(STOP[lang])
        elif lang == "ko":
            out.append(" ")
    return "".join(out) + "\n"


def run(binary, folder):
    out = subprocess.run([binary, "graph", str(folder), "-", "--no-cache"], capture_output=True, check=True).stdout
    return json.loads(out)


def score(binary, root, truth):
    doc_ = run(binary, root)
    nodes = {n["id"]: n for n in doc_["nodes"]}
    linked, crossing, total_links = set(), 0, 0
    for l in doc_["links"]:
        if l["relation"] != "similar_to":
            continue
        total_links += 1
        a, b = l["source"], l["target"]
        if truth[a] == truth[b]:
            linked.add(a)
            linked.add(b)
        else:
            crossing += 1
    return len(linked), crossing, total_links


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default="target/release/sniff-rs")
    ap.add_argument("--baseline")
    ap.add_argument("--docs", type=int, default=10)
    args = ap.parse_args()
    rng = random.Random(7)
    bad = 0
    with tempfile.TemporaryDirectory() as tmp:
        for lang, topics in TOPICS.items():
            root = Path(tmp) / lang
            root.mkdir()
            truth = {}
            for topic, words in topics.items():
                for k in range(args.docs):
                    name = f"{topic}_{k}.txt"
                    (root / name).write_text(doc(rng, lang, rng.sample(words, 8)), encoding="utf-8")
                    truth[name] = topic
            n = len(truth)
            linked, crossing, links = score(args.bin, root, truth)
            line = f"{lang}: {linked}/{n} documents linked to their own topic, {crossing} links across topics, {links} similar_to links"
            if args.baseline:
                l0, c0, k0 = score(args.baseline, root, truth)
                line += f"  (baseline: {l0}/{n}, {c0} across, {k0} links)"
            print(line)
            if linked < n * 0.9 or crossing:
                bad += 1
    print("ok" if not bad else f"{bad} language(s) below the bar")
    raise SystemExit(1 if bad else 0)


if __name__ == "__main__":
    main()
