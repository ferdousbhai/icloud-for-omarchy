QT += core gui printsupport
CONFIG += c++17 console
CONFIG -= app_bundle
TARGET = backend_test
TEMPLATE = app

SOURCES += backend_test.cpp \
    ../src/notesbackend.cpp
HEADERS += ../src/notesbackend.h
INCLUDEPATH += ../src
