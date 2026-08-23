import rspack from '@rspack/core';
import { VueLoaderPlugin } from 'vue-loader';

export default {
    experiments: { css: true },
    entry: { main: './src/main.js' },
    resolve: { extensions: ['.js', '.vue'] },
    module: {
        rules: [
            { test: /\.vue$/, loader: 'vue-loader', options: { experimentalInlineMatchResource: true } },
            { test: /\.css$/, type: 'css/auto' },
        ],
    },
    plugins: [
        new VueLoaderPlugin(),
        new rspack.HtmlRspackPlugin({ template: './index.html' }),
    ],
    devServer: {
        port: 4000,
        hot: true,
        historyApiFallback: true,
    },
    output: { publicPath: '/' },
};

