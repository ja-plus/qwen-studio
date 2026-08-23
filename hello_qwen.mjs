import OpenAI from "openai";

const openai = new OpenAI({
  apiKey: process.env.DASHSCOPE_API_KEY,
  baseURL: "https://dashscope.aliyuncs.com/compatible-mode/v1"
});

// const completion = await openai.chat.completions.create({
//   model: "qwen3.8-max",
//   messages: [
//     { role: "user", content: "你好， 我正在调用千问api" }
//   ]
// });
const completion = await openai.chat.completions.create({
  model: "deepseek-v4-pro-0813",
  messages: [
    { role: "user", content: "你好， 我正在调用千问api" }
  ]
});

console.log(completion.choices[0].message.content);