// good-cat — FlClash 覆写脚本，入口 main(config)。
// 按节点名称识别国家；可分别修改下面两个数组。
// 保留订阅节点、proxy-providers 及 FlClash 默认 GeoSite 数据源；替换所有分组和规则。
const CHATGPT_COUNTRIES = ["US", "JP", "SG", "TW"];
const CLAUDE_COUNTRIES = ["US", "JP", "SG", "TW"];

const COUNTRY_PATTERNS = {
  US: "美国|美國|美利坚|美利堅|洛杉矶|洛杉磯|圣何塞|聖何塞|西雅图|西雅圖|纽约|紐約|🇺🇸|\\b(?:US|USA|United States)\\b",
  JP: "日本|东京|東京|大阪|🇯🇵|\\b(?:JP|Japan)\\b",
  SG: "新加坡|狮城|獅城|🇸🇬|\\b(?:SG|Singapore)\\b",
  TW: "台湾|台灣|臺灣|台北|臺北|🇹🇼|\\b(?:TW|Taiwan)\\b",
  KR: "韩国|韓國|首尔|首爾|🇰🇷|\\b(?:KR|Korea)\\b",
  GB: "英国|英國|伦敦|倫敦|🇬🇧|\\b(?:GB|UK|Britain|United Kingdom)\\b",
  DE: "德国|德國|法兰克福|法蘭克福|🇩🇪|\\b(?:DE|Germany)\\b",
  CA: "加拿大|🇨🇦|\\b(?:CA|Canada)\\b",
  AU: "澳大利亚|澳大利亞|澳洲|悉尼|雪梨|🇦🇺|\\b(?:AU|Australia)\\b",
  HK: "香港|🇭🇰|\\b(?:HK|Hong Kong)\\b"
};

function countryFilter(countries) {
  if (!Array.isArray(countries) || countries.length === 0) {
    throw new Error("good-cat: 国家列表不能为空");
  }
  const patterns = countries.map(function (code) {
    if (!Object.prototype.hasOwnProperty.call(COUNTRY_PATTERNS, code)) {
      throw new Error("good-cat: 未定义的国家代码 " + code);
    }
    return COUNTRY_PATTERNS[code];
  });
  // Mihomo 使用 Go/RE2 正则，(?i) 为不区分大小写。
  return "(?i)(" + patterns.join("|") + ")";
}

function main(config) {
  if (!config || typeof config !== "object" || Array.isArray(config)) {
    throw new Error("good-cat: 配置必须是对象");
  }
  const chatgptGroupName = "🤖 ChatGPT";
  const claudeGroupName = "🧠 Claude";
  const proxyGroupName = "🌐 Porxy";

  function urlTest(name, url, countries) {
    const group = {
      name: name,
      type: "url-test",
      "include-all": true,
      url: url,
      interval: 300,
      tolerance: 50,
      lazy: true
    };
    if (countries) group.filter = countryFilter(countries);
    return group;
  }

  config["proxy-groups"] = [
    urlTest(chatgptGroupName, "https://chatgpt.com/cdn-cgi/trace", CHATGPT_COUNTRIES),
    urlTest(claudeGroupName, "https://claude.ai/cdn-cgi/trace", CLAUDE_COUNTRIES),
    urlTest(proxyGroupName, "https://www.gstatic.com/generate_204")
  ];

  // 本地域名和私有 IP 使用数据源维护的 private 分类，不重复枚举网段。
  const privateRules = [
    "GEOSITE,private,DIRECT",
    "GEOIP,private,DIRECT,no-resolve"
  ];
  const chatgptRules = [
    `GEOSITE,openai,${chatgptGroupName}`
  ];
  const claudeRules = [
    `GEOSITE,anthropic,${claudeGroupName}`
  ];
  const directRules = [
    "GEOSITE,microsoft,DIRECT",
    "GEOSITE,apple,DIRECT"
  ];
  const chinaRules = [
    "GEOSITE,cn,DIRECT",
    "GEOIP,CN,DIRECT,no-resolve"
  ];

  // 直接赋值，彻底清空原规则；AI 规则优先于微软/苹果和中国规则。
  config.rules = [].concat(
    privateRules, chatgptRules, claudeRules, directRules, chinaRules,
    [`MATCH,${proxyGroupName}`]
  );
  // 原规则集和子规则不再引用，避免继续下载订阅附带的规则。
  delete config["rule-providers"];
  delete config["sub-rules"];
  config.mode = "rule";
  return config;
}
