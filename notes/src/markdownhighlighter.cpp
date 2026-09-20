#include "markdownhighlighter.h"

#include <QRegularExpression>
#include <QTextCharFormat>

MarkdownHighlighter::MarkdownHighlighter(QTextDocument *document, const Colors &colors)
    : QSyntaxHighlighter(document), m_colors(colors)
{
}

void MarkdownHighlighter::setColors(const Colors &colors)
{
    m_colors = colors;
    rehighlight();
}

void MarkdownHighlighter::highlightBlock(const QString &text)
{
    static const QRegularExpression heading(QStringLiteral(R"(^(#{1,6})\s+)"));
    static const QRegularExpression checkbox(QStringLiteral(R"(^\s*(?:[-*+]|\d+[.)])\s+(\[( |x|X)\])\s)"));
    static const QRegularExpression listMarker(QStringLiteral(R"(^\s*([-*+]|\d+[.)])\s)"));
    static const QRegularExpression quote(QStringLiteral(R"(^\s*>)"));
    static const QRegularExpression rule(QStringLiteral(R"(^\s*([-*_])\s*\1\s*\1[\s\-*_]*$)"));
    static const QRegularExpression bold(QStringLiteral(R"((\*\*|__)(?=\S)(.+?)(?<=\S)\1)"));
    static const QRegularExpression italic(QStringLiteral(R"((?<![*\w])(\*|_)(?=\S)([^*_]+?)(?<=\S)\1(?![*\w]))"));
    static const QRegularExpression strike(QStringLiteral(R"(~~(?=\S)(.+?)(?<=\S)~~)"));
    static const QRegularExpression code(QStringLiteral(R"(`([^`]+)`)"));
    static const QRegularExpression link(QStringLiteral(R"((!?\[)([^\]]*)(\]\([^)]*\)))"));

    QTextCharFormat mutedFormat;
    mutedFormat.setForeground(m_colors.muted);
    QTextCharFormat accentFormat;
    accentFormat.setForeground(m_colors.accent);

    // Line shapes first, so inline emphasis inside them still shows.
    QRegularExpressionMatch m = heading.match(text);
    if (m.hasMatch()) {
        QTextCharFormat headingFormat;
        headingFormat.setFontWeight(QFont::Bold);
        const int level = int(m.captured(1).size());
        headingFormat.setFontPointSize(document()->defaultFont().pointSizeF() * (level == 1 ? 1.35 : level == 2 ? 1.2 : 1.08));
        setFormat(0, int(text.size()), headingFormat);
        setFormat(0, int(m.capturedLength(0)), mutedFormat);
    } else if ((m = rule.match(text)).hasMatch()) {
        setFormat(0, int(text.size()), mutedFormat);
    } else if ((m = quote.match(text)).hasMatch()) {
        QTextCharFormat quoteFormat;
        quoteFormat.setForeground(m_colors.dim);
        quoteFormat.setFontItalic(true);
        setFormat(0, int(text.size()), quoteFormat);
        setFormat(0, int(m.capturedLength(0)), accentFormat);
    } else if ((m = checkbox.match(text)).hasMatch()) {
        if (m.captured(2) != u" ") {
            QTextCharFormat doneFormat;
            doneFormat.setForeground(m_colors.muted);
            doneFormat.setFontStrikeOut(true);
            setFormat(int(m.capturedEnd(1)), int(text.size() - m.capturedEnd(1)), doneFormat);
        }
        setFormat(0, int(m.capturedEnd(1)), accentFormat);
    } else if ((m = listMarker.match(text)).hasMatch()) {
        setFormat(int(m.capturedStart(1)), int(m.capturedLength(1)), accentFormat);
    }

    auto eachMatch = [&](const QRegularExpression &re, auto apply) {
        for (const QRegularExpressionMatch &match : re.globalMatch(text))
            apply(match);
    };
    eachMatch(bold, [&](const QRegularExpressionMatch &match) {
        QTextCharFormat f = format(int(match.capturedStart(2)));
        f.setFontWeight(QFont::Bold);
        setFormat(int(match.capturedStart(2)), int(match.capturedLength(2)), f);
        setFormat(int(match.capturedStart(0)), int(match.capturedLength(1)), mutedFormat);
        setFormat(int(match.capturedEnd(2)), int(match.capturedLength(1)), mutedFormat);
    });
    eachMatch(italic, [&](const QRegularExpressionMatch &match) {
        QTextCharFormat f = format(int(match.capturedStart(2)));
        f.setFontItalic(true);
        setFormat(int(match.capturedStart(2)), int(match.capturedLength(2)), f);
        setFormat(int(match.capturedStart(0)), 1, mutedFormat);
        setFormat(int(match.capturedEnd(2)), 1, mutedFormat);
    });
    eachMatch(strike, [&](const QRegularExpressionMatch &match) {
        QTextCharFormat f = format(int(match.capturedStart(1)));
        f.setFontStrikeOut(true);
        setFormat(int(match.capturedStart(1)), int(match.capturedLength(1)), f);
        setFormat(int(match.capturedStart(0)), 2, mutedFormat);
        setFormat(int(match.capturedEnd(1)), 2, mutedFormat);
    });
    eachMatch(code, [&](const QRegularExpressionMatch &match) {
        QTextCharFormat f;
        f.setFontFamilies({ QStringLiteral("monospace") });
        f.setBackground(m_colors.code);
        setFormat(int(match.capturedStart(0)), int(match.capturedLength(0)), f);
    });
    eachMatch(link, [&](const QRegularExpressionMatch &match) {
        setFormat(int(match.capturedStart(1)), int(match.capturedLength(1)), mutedFormat);
        QTextCharFormat f = format(int(match.capturedStart(2)));
        f.setForeground(m_colors.accent);
        f.setFontUnderline(true);
        setFormat(int(match.capturedStart(2)), int(match.capturedLength(2)), f);
        setFormat(int(match.capturedStart(3)), int(match.capturedLength(3)), mutedFormat);
    });
}
