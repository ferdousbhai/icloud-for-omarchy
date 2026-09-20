QT += core gui quick printsupport
CONFIG += c++17 console
CONFIG -= app_bundle
TARGET = backend_test
TEMPLATE = app

SOURCES += backend_test.cpp \
    ../src/notesbackend.cpp \
    ../src/markdownhighlighter.cpp
HEADERS += ../src/notesbackend.h ../src/markdownhighlighter.h
INCLUDEPATH += ../src
